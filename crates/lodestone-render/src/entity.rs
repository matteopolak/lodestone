//! The entity render plan: the version-free layer that turns *tracked entities*
//! into *a culled, instanced set of draws* for one frame.
//!
//! Entities are the second geometry pipeline (terrain being the first). They are
//! fundamentally different from blocks and reuse none of the mesher:
//!
//! * Their geometry is a **cuboid part hierarchy**, not paletted voxels, and it
//!   is already baked version-free by [`lodestone_assets::entity::bake_entity`]
//!   from the hand-ported [`entity_models`](lodestone_assets::entity_models)
//!   corpus. This module never re-derives geometry; it converts the baked quads
//!   into the shared [`ModelVertex`] format once per model type.
//! * They are **instanced**: a mob farm is hundreds of the same model at
//!   different transforms, so the mesh is uploaded once and each visible entity
//!   contributes only a per-instance model matrix (see
//!   [`crate::entity_pipeline`]). This module produces those matrices.
//!
//! # The placement transform (why it is exactly this and not eyeballed)
//!
//! Vanilla places a living entity with a fixed sequence of pose-stack ops,
//! read here from the decompiled 26.2 client:
//!
//! ```text
//!   translate(feetPos)                     // move to the entity's feet
//!   rotateY(180° - bodyYaw)                // face the body's yaw
//!   scale(-1, -1, 1)                       // model space is Y-down, Z-forward
//!   translate(0, -1.501, 0)                // lift feet to the ground plane
//! ```
//!
//! Model space has **Y pointing down** (the head cube spans `y ∈ [-8, 0]`, the
//! feet reach `y = +24`), which is why the `scale(-1, -1, 1)` flip is load
//! bearing rather than cosmetic: without it every mob renders upside down but
//! still recognisable, the exact "looks plausible, is wrong" trap. The
//! composition order is copied from source, not inferred, and
//! [`entity_model_matrix`] is unit-tested to put feet on the ground and the head
//! above them. The `scale(-1,-1,1)` has determinant `+1`, so it preserves
//! winding — a front face in model space stays a front face in world space, and
//! back-face culling remains valid.
//!
//! Per-part animation (head tracking, walk cycles) is a layer *above* this: it
//! adjusts each [`PartPose`](lodestone_assets::entity::PartPose) before baking.
//! This module renders the static rest pose posed only by body yaw, which is
//! what the incoming [`EntityView`](../../lodestone_client/state/struct.EntityView.html)
//! data supports today (position + rotation, no limb angles).

use glam::{Mat4, Vec3, Vec4};
use lodestone_assets::entity::{EntityModelDef, bake_entity_parts};
use lodestone_assets::entity_models::{EntityModelEntry, entity_models};
use lodestone_assets::equipment::{ArmourLayer, ArmourSlot, armour_item, humanoid_armour_model};
use lodestone_assets::{BakedQuad, DisplaySlot, DisplayTransform, DisplayTransforms, GuiLight};
use lodestone_data::entity_type::EntityType;
use lodestone_model::{CampfireSlot, EntityNetworkId, ShelfSlot};

use crate::camera::Frustum;
use crate::entity_anim::{AnimInput, HandPoseOverride, HumanoidArms, Skeleton};
use crate::item_render::{UNITS_PER_BLOCK, display_matrix, display_matrix_for_hand};
use crate::models::{ModelMesh, ModelVertex, mesh_item_quads};

#[path = "entity_catalog.rs"]
mod entity_catalog;
#[path = "entity_model.rs"]
mod entity_model;
#[path = "entity_batch.rs"]
mod entity_batch;
#[path = "entity_layers.rs"]
mod entity_layers;
#[path = "entity_item.rs"]
mod entity_item;
#[path = "entity_orb.rs"]
mod entity_orb;
#[path = "entity_first_person.rs"]
mod entity_first_person;

pub use entity_catalog::*;
pub use entity_model::*;
pub use entity_batch::*;
pub use entity_layers::*;
pub use entity_item::*;
pub use entity_orb::*;
pub use entity_first_person::*;

pub(crate) use entity_catalog::canonical_model_name;
#[cfg(test)]
pub(crate) use entity_catalog::boat_model_name;
pub(crate) use entity_model::push_part_quads;
pub(crate) use entity_item::mesh_item_quads_with_light;
#[cfg(test)]
pub(crate) use entity_layers::SHEEP_WOOL_PART_NAMES;
#[cfg(test)]
pub(crate) use entity_orb::experience_orb_cell_uvs;
#[cfg(test)]
pub(crate) use entity_first_person::ArmSwingTerms;

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::Direction;

    fn pig_mesh() -> EntityMesh {
        EntityMesh::from_model(&lodestone_assets::entity_models::pig_model())
    }

    // -----------------------------------------------------------------------
    // Humanoid armour
    // -----------------------------------------------------------------------

    /// Every armour slot must bake real geometry, and every *load-bearing* part
    /// it bakes must attach to a real part of the humanoid rigs that wear
    /// armour. An armour mesh whose parts do not attach draws nothing at all —
    /// the island defect, with a green mesh test.
    ///
    /// `hat` is the one excusable miss: it is the helmet's outermost shell, it
    /// unwraps onto a region measured empty in all nine of 26.2's humanoid
    /// armour sheets, and the corpus `armor_stand` rig deliberately has no `hat`
    /// part at all (vanilla forces `hat.visible = false` there). So it is
    /// required to attach *only* where the wearer has one — which is itself an
    /// assertion, not a shrug.
    #[test]
    fn every_armour_slot_attaches_to_every_humanoid_rig() {
        let set = ArmourModelSet::load();
        let models = EntityModelSet::load();
        for wearer_name in [
            "player_wide",
            "player_slim",
            "zombie",
            "skeleton",
            "armor_stand",
        ] {
            let wearer = models
                .get(wearer_name)
                .unwrap_or_else(|| panic!("{wearer_name} must be in the corpus"));
            assert!(
                wearer_carries_armour(&wearer.skeleton),
                "{wearer_name} must classify as humanoid, or it wears nothing"
            );
            for (slot, mesh) in set.iter() {
                assert!(mesh.quad_count() > 0, "{slot:?} baked no geometry at all");
                let attached: Vec<&'static str> = mesh
                    .attach(&wearer.skeleton)
                    .map(|(range, wearer_index)| {
                        assert!(range.index_count > 0, "{slot:?} attached an empty range");
                        assert!(wearer_index < wearer.skeleton.len());
                        mesh.parts
                            .iter()
                            .find(|(_, r)| *r == range)
                            .map(|(n, _)| *n)
                            .expect("range came from this mesh")
                    })
                    .collect();
                let expected: Vec<&'static str> = mesh
                    .parts
                    .iter()
                    .map(|(n, _)| *n)
                    .filter(|n| *n != "hat" || wearer.skeleton.index_of("hat").is_some())
                    .collect();
                assert_eq!(
                    attached, expected,
                    "{wearer_name} cannot carry every part of {slot:?}"
                );
            }
        }
    }

    /// **The water-clip mask reaches `resolve_animated` through the real
    /// corpus loader, not just through its own standalone builder.** Owner
    /// report: "placing down a boat still shows water through the bottom".
    /// This is the render-layer half of the island check
    /// `lodestone_assets::entity_models`'s own `the_water_patch_is_a_real_corpus_entry`
    /// makes at the data layer: a name present in `entity_models()` still has
    /// to survive `EntityModelSet::load()`'s baking and `canonical_model_name`'s
    /// resolution before anything can draw it.
    #[test]
    fn boat_water_patch_resolves_through_the_real_corpus_loader() {
        let models = EntityModelSet::load();
        let anim = AnimInput::REST;
        let instance = models
            .resolve_animated("boat_water_patch", Vec3::new(1.0, 64.0, 2.0), 30.0, 0.0, 1.0, &anim, 0.0, 0.0)
            .expect(
                "\"boat_water_patch\" must resolve through the same corpus loader every real \
                 boat instance goes through, or the fix is present in source and reaches no pixel",
            );
        assert_eq!(instance.model, "boat_water_patch");
    }

    /// **The mask must sit exactly where the boat itself is drawn, not merely
    /// somewhere plausible.** Vanilla's base boat renderer submit function calls
    /// its type-additions submit step (the water-patch submit) *inside* the same
    /// push/pop pose block as the main model, after the identical
    /// bob/rotate/flip/spin sequence — so the two must share one placement
    /// transform, not two similar ones. Both hypotheses are checked: the
    /// right one (`"boat_water_patch"` joins `non_living_vehicle_placement`'s
    /// `"boat"` arm) and the wrong one this fix could easily have shipped
    /// (falling through to the *living-entity* placement — a bare
    /// `resolve`/`resolve_posed` matrix with no `0.375` bob and no `90°`
    /// spin), which would leave the mask floating at the wrong height and
    /// facing broadside to the hull it exists to seal.
    #[test]
    fn the_water_patch_shares_the_boats_own_placement_transform() {
        let models = EntityModelSet::load();
        let anim = AnimInput::REST;
        let feet = Vec3::new(-4.0, 70.0, 11.0);
        let boat = models
            .resolve_animated("boat", feet, 217.0, 0.0, 1.0, &anim, 0.0, 0.0)
            .expect("\"boat\" must resolve");
        let patch = models
            .resolve_animated("boat_water_patch", feet, 217.0, 0.0, 1.0, &anim, 0.0, 0.0)
            .expect("\"boat_water_patch\" must resolve");
        assert_eq!(
            boat.transform, patch.transform,
            "the mask's placement transform must be bit-identical to the boat's own, not a \
             separately-derived approximation"
        );

        // The wrong hypothesis, computed from the *other* placement rule this
        // fix could have fallen through to (a living-entity matrix, no bob,
        // no spin) — must disagree, or this test cannot tell the two apart.
        let living_matrix = crate::entity::EntityInstance::new(
            "boat_water_patch",
            models.get("boat_water_patch").expect("resolved above"),
            feet,
            217.0,
            1.0,
            &anim,
        )
        .transform;
        assert_ne!(
            boat.transform, living_matrix,
            "the living-entity placement must differ from the vehicle one at this yaw, or \
             the positive assertion above proves nothing"
        );
    }

    /// A non-humanoid rig carries no armour, and that is the correct answer
    /// rather than a fallback: vanilla's humanoid armour layer is only attached to
    /// renderers whose model is a base humanoid model, so a pig handed a chestplate
    /// by a plugin wears nothing in vanilla either.
    ///
    /// The negative control matters here: a pig **does** have `head` and
    /// `body`, so a name-keyed attach would happily bolt a chestplate to it.
    /// That is why the gate is the animation family, and why this asserts the
    /// name lookup would otherwise have succeeded.
    #[test]
    fn a_pig_attaches_no_armour_despite_having_a_body_part() {
        let set = ArmourModelSet::load();
        let pig = pig_mesh();
        assert!(!wearer_carries_armour(&pig.skeleton));
        assert!(
            pig.skeleton.index_of("body").is_some() && pig.skeleton.index_of("head").is_some(),
            "control: the pig must have the parts a name-keyed attach would match"
        );
        for (_, mesh) in set.iter() {
            assert_eq!(mesh.attach(&pig.skeleton).count(), 0);
        }
    }

    // -----------------------------------------------------------------------
    // Sheep wool
    // -----------------------------------------------------------------------

    fn cow_mesh() -> EntityMesh {
        EntityMesh::from_model(&lodestone_assets::entity_models::cow_model())
    }

    fn sheep_mesh() -> EntityMesh {
        EntityMesh::from_model(&lodestone_assets::entity_models::sheep_model())
    }

    /// A sheep attaches every one of the wool mesh's six parts to its own
    /// body — the positive half of the pig/cow trap check below: if this did
    /// not attach, the negative checks would be proving nothing.
    #[test]
    fn a_sheep_attaches_every_wool_part_to_its_own_body() {
        let wool = WoolMesh::load();
        let sheep = sheep_mesh();
        assert_eq!(wool.parts.len(), 6, "sheep_wool_model must bake all six named parts");
        let attached: Vec<_> = wool.attach(&sheep.skeleton, "sheep").collect();
        assert_eq!(
            attached.len(),
            6,
            "every wool part must attach to the real sheep body rig"
        );
        for (range, wearer_index) in &attached {
            assert!(range.index_count > 0, "an attached wool part baked no geometry");
            assert!(*wearer_index < sheep.skeleton.len());
        }
    }

    /// **The pig/cow trap, for wool.** `sheep`, `pig`, `cow` and `wolf` are all
    /// `AnimFamily::Quadruped` and all four share the exact part *names*
    /// [`sheep_wool_model`] uses (`head`, `body`, `*_hind_leg`, `*_front_leg`)
    /// — `quadruped_root` builds every one of them from the same generator.
    /// So a pig or a cow genuinely **does** have every name [`WoolMesh::attach`]
    /// looks up, which is exactly why gating on `wearer.family()` (armour's own
    /// discipline) would be wrong here: it would resolve cleanly and grow a
    /// fleece on a farm animal. The control matters for the same reason
    /// `a_pig_attaches_no_armour_despite_having_a_body_part` asserts it does:
    /// without it, this test could pass by accident (a rig with no matching
    /// parts at all) rather than by the `wearer_model` gate actually working.
    #[test]
    fn a_pig_and_a_cow_attach_no_wool_despite_sharing_every_part_name() {
        let wool = WoolMesh::load();
        for (name, mesh) in [("pig", pig_mesh()), ("cow", cow_mesh())] {
            for part_name in SHEEP_WOOL_PART_NAMES {
                assert!(
                    mesh.skeleton.index_of(part_name).is_some(),
                    "control: {name} must have a {part_name} part, or this test proves \
                     nothing about the wearer_model gate specifically"
                );
            }
            // The real would-be-wrong call: gating on family alone, exactly the
            // mistake `docs/entity-rendering.md` names.
            assert_eq!(
                mesh.skeleton.family(),
                crate::entity_anim::AnimFamily::Quadruped,
                "{name} must share the sheep's animation family for this control to be real"
            );
            assert_eq!(
                wool.attach(&mesh.skeleton, name).count(),
                0,
                "{name} must attach no wool part when gated on its own resolved model name"
            );
        }
    }

    /// The armour a wearer draws with is *its own* posed part matrix, so the
    /// world-pose determinant invariant is inherited rather than re-derived:
    /// every matrix an armour layer is drawn under has to be **positive**,
    /// orientation-preserving like any world model matrix, so that composing it
    /// with `view_projection` leaves the camera's own sign untouched and the
    /// same faces survive culling as for un-armoured geometry.
    ///
    /// The camera's own polarity is deliberately not asserted. It follows from
    /// which end of `[0, 1]` the near plane sits at — negative under a forward
    /// projection, positive under this renderer's reversed-Z one — and it is not
    /// what the rasterizer reads. The claim that matters is that the *pose* does
    /// not reverse orientation, and that is absolute.
    #[test]
    fn armour_is_drawn_under_positive_determinant_wearer_matrices() {
        let camera = crate::camera::Camera::default();
        let view_proj = camera.view_projection().determinant();
        assert!(
            view_proj.abs() > 1.0e-6,
            "the reference camera's projection is degenerate ({view_proj}), so \
             composition through it says nothing"
        );

        let set = ArmourModelSet::load();
        let models = EntityModelSet::load();
        let instance = models
            .resolve("zombie", Vec3::new(3.0, 64.0, -7.0), 37.0, 1.0, &AnimInput {
                head_yaw_deg: 12.0,
                head_pitch_deg: -8.0,
                limb_swing: 3.5,
                limb_swing_amount: 0.9,
                attack_anim: 0.4,
                age_ticks: 42.0,
                aggressive: false,
                ..AnimInput::REST
            })
            .expect("zombie resolves");
        let mesh = models.get("zombie").expect("zombie mesh");
        let mut checked = 0;
        for (_, armour) in set.iter() {
            for (_, wearer_index) in armour.attach(&mesh.skeleton) {
                let m = instance.part_transforms[wearer_index];
                assert!(
                    m.determinant() > 0.0,
                    "armour part matrix determinant must be positive, was {}",
                    m.determinant()
                );
                // And the composed clip transform must then inherit the
                // camera's sign, which is what actually decides facing.
                assert_eq!(
                    (camera.view_projection() * m).determinant().signum(),
                    view_proj.signum()
                );
                checked += 1;
            }
        }
        assert!(checked >= 8, "only {checked} armour parts checked");
    }

    /// Layer resolution: two coplanar layers for leather (base + overlay), one
    /// for a plain material, none across slots, none for the head-slot items
    /// vanilla draws through some other layer.
    #[test]
    fn armour_layer_resolution_follows_the_item_and_its_slot() {
        assert_eq!(armour_layers(ArmourSlot::Chest, "leather_chestplate").len(), 2);
        assert_eq!(armour_layers(ArmourSlot::Legs, "leather_leggings").len(), 2);
        assert_eq!(armour_layers(ArmourSlot::Head, "diamond_helmet").len(), 1);
        assert_eq!(armour_layers(ArmourSlot::Head, "turtle_helmet").len(), 1);
        // A helmet forced into the boots slot draws nothing, as
        // vanilla's own should-render slot-equality check demands.
        assert!(armour_layers(ArmourSlot::Feet, "diamond_helmet").is_empty());
        // Not armour at all.
        assert!(armour_layers(ArmourSlot::Head, "carved_pumpkin").is_empty());
        assert!(armour_layers(ArmourSlot::Chest, "elytra").is_empty());
        assert!(armour_layers(ArmourSlot::Chest, "wolf_armor").is_empty());
        assert!(armour_layers(ArmourSlot::Head, "stone").is_empty());
    }

    /// Only leather's base layer is tinted, and it is tinted to vanilla's
    /// `color_when_undyed`. White for everything else — a tint of `[0,0,0]`
    /// would be black armour and a tint applied to the overlay would recolour
    /// the buckles.
    #[test]
    fn only_leathers_base_layer_carries_a_tint() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        assert_eq!(
            armour_layer_tint(&leather[0]),
            lodestone_assets::equipment::UNDYED_LEATHER_RGB
        );
        assert_eq!(armour_layer_tint(&leather[1]), [255, 255, 255]);
        let diamond = armour_layers(ArmourSlot::Head, "diamond_helmet");
        assert_eq!(armour_layer_tint(&diamond[0]), [255, 255, 255]);
    }

    /// A real `minecraft:dyed_color` reaches the base leather layer
    /// unchanged (mod the alpha byte vanilla's opaque-forcing helper strips), while the
    /// non-dyeable overlay layer ignores it exactly as
    /// vanilla's per-layer colour function's `else -> -1` branch does — two competing
    /// hypotheses (dye applied vs. dye ignored) landing on different layers
    /// of the *same* item, so a broken layer/dye pairing cannot pass by
    /// accident.
    #[test]
    fn a_real_dye_reaches_the_base_layer_but_not_the_overlay() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        // Bright cyan (`0x00FFFF`), chosen only for being nowhere near
        // `UNDYED_LEATHER_RGB` (`0xA06540`) so a fallback-to-undyed bug is
        // unmistakable.
        let dye = Some(0x0000_FFFF_u32);
        assert_eq!(armour_layer_tint_with_dye(&leather[0], dye), [0x00, 0xFF, 0xFF]);
        // The overlay has no dyeable block: vanilla's per-layer colour function
        // never reads
        // its dye-colour field for it.
        assert_eq!(armour_layer_tint_with_dye(&leather[1], dye), [255, 255, 255]);
    }

    /// `dyed_color: None` (component absent) falls back to
    /// vanilla's dyeable-item default-colour accessor, matching the zero-argument
    /// [`armour_layer_tint`] this delegates to.
    #[test]
    fn absent_dye_falls_back_to_color_when_undyed() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        assert_eq!(
            armour_layer_tint_with_dye(&leather[0], None),
            lodestone_assets::equipment::UNDYED_LEATHER_RGB
        );
        assert_eq!(
            armour_layer_tint_with_dye(&leather[0], None),
            armour_layer_tint(&leather[0])
        );
    }

    /// The vanilla quirk pinned in [`armour_layer_tint_with_dye`]'s own
    /// doc: dyeing leather pure black is indistinguishable from not dyeing
    /// it at all, because vanilla's dyed-item-color default lookup forces the
    /// colour opaque, which only
    /// touches the alpha byte, so a black dye's RGB portion is still `0` and
    /// `dyeColor != 0` reads false. This is vanilla's own equipment-layer renderer's own
    /// behaviour, not a port bug — a
    /// "fix" that special-cases black would diverge from the game it ports.
    #[test]
    fn dyed_color_zero_reads_as_undyed() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        assert_eq!(
            armour_layer_tint_with_dye(&leather[0], Some(0x0000_0000)),
            lodestone_assets::equipment::UNDYED_LEATHER_RGB
        );
    }

    /// A non-dyeable layer ignores `dyed_color` even when one is present —
    /// vanilla's per-layer colour function's `else` branch never reads its dye-colour field at all, so
    /// a diamond helmet dyed (nonsensically) any colour still draws white.
    #[test]
    fn non_dyeable_layers_ignore_a_present_dye() {
        let diamond = armour_layers(ArmourSlot::Head, "diamond_helmet");
        assert_eq!(
            armour_layer_tint_with_dye(&diamond[0], Some(0x00FF_0000)),
            [255, 255, 255]
        );
    }

    /// The two vanilla anchor values, hand-derived from the real timeline
    /// keyframes rather than from this implementation,
    /// so agreement is evidence rather than a tautology:
    ///
    /// * noon (6000) falls inside the `[730, 11270)` plateau segment, both of
    ///   whose keyframes are `1.0` — constant `1.0` regardless of where in the
    ///   segment 6000 lands.
    /// * midnight (18000) falls inside the `[13140, 22860)` plateau segment,
    ///   both of whose keyframes are `0.24` — constant `0.24` likewise.
    ///
    /// Both are covered far more thoroughly, tick-by-tick against a real JVM,
    /// by `tests/sky_light_factor_timeline.rs`; these two stay as a fast
    /// same-crate smoke check.
    #[test]
    fn sky_darken_hits_vanillas_noon_and_midnight_anchors() {
        assert!((sky_darken_for_time_of_day(6_000) - 1.0).abs() < 1e-5);
        assert!((sky_darken_for_time_of_day(18_000) - 0.24).abs() < 1e-5);
    }

    /// A large world age must reduce into the day, not drift: `time_of_day`
    /// keeps counting past 24000 for the life of a world, and a curve that read
    /// it raw would eventually saturate at one end and stop darkening at all —
    /// a bug that only appears on a world that has been running for a while,
    /// i.e. never in a test and always for the player.
    #[test]
    fn sky_darken_reduces_a_large_world_age_into_the_day() {
        assert_eq!(
            sky_darken_for_time_of_day(18_000),
            sky_darken_for_time_of_day(18_000 + 24_000 * 500)
        );
        assert_eq!(
            sky_darken_for_time_of_day(6_000),
            sky_darken_for_time_of_day(6_000 - 24_000 * 500)
        );
    }

    /// The curve must stay inside vanilla's `[0.24, 1.0]` across a whole day and
    /// must actually *vary* — a constant 1.0 is the shipped bug. The renderer's
    /// negative unset sentinel is outside this curve's range, so zero remains
    /// available for an explicit dimension factor.
    #[test]
    fn sky_darken_stays_in_vanillas_range_and_is_not_constant() {
        let samples: Vec<f32> = (0..24_000)
            .step_by(50)
            .map(sky_darken_for_time_of_day)
            .collect();
        let lo = samples.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(lo >= 0.24 - 1e-5, "dipped to {lo}, below vanilla's 0.24 floor");
        assert!(hi <= 1.0 + 1e-5, "rose to {hi}, above 1.0");
        assert!(lo > 0.0, "the ordinary time-of-day curve must stay above zero");
        assert!(hi - lo > 0.5, "the curve barely moves ({lo}..{hi}) — that is the defect");
    }

    #[test]
    fn maps_known_entity_types_to_models() {
        assert_eq!(model_for_type(EntityType::Pig).unwrap().name, "pig");
        assert_eq!(model_for_type(EntityType::Cow).unwrap().name, "cow");
        assert_eq!(model_for_type(EntityType::Chicken).unwrap().name, "chicken");
        assert_eq!(model_for_type(EntityType::Sheep).unwrap().name, "sheep");
        assert_eq!(model_for_type(EntityType::Zombie).unwrap().name, "zombie");
        assert_eq!(model_for_type(EntityType::Skeleton).unwrap().name, "skeleton");
        assert_eq!(model_for_type(EntityType::Creeper).unwrap().name, "creeper");
        assert_eq!(model_for_type(EntityType::Spider).unwrap().name, "spider");
        // The two surviving aliases: a type path that is not a corpus name.
        assert_eq!(model_for_type(EntityType::Player).unwrap().name, "player_wide");
        assert_eq!(model_for_type(EntityType::Bogged).unwrap().name, "skeleton");
    }

    /// The reported defect: a drowned rendered as an ordinary zombie. Its mesh
    /// and its sheet both exist in the corpus; a stale alias in this module was
    /// routing it to the zombie's. Every mob here is one that alias table used
    /// to swallow, so each assertion is a distinct wrong-mesh substitution.
    #[test]
    fn mob_variants_resolve_to_their_own_model_not_a_base_mob() {
        for (entity_type, wrong) in [
            (EntityType::Drowned, "zombie"),
            (EntityType::Husk, "zombie"),
            (EntityType::ZombieVillager, "zombie"),
            (EntityType::Stray, "skeleton"),
            (EntityType::WitherSkeleton, "skeleton"),
            (EntityType::CaveSpider, "spider"),
            (EntityType::Mooshroom, "cow"),
        ] {
            let ty = entity_type.path();
            let model =
                model_for_type(entity_type).unwrap_or_else(|| panic!("{ty} has a corpus model"));
            assert_eq!(
                model.name, ty,
                "{ty} resolved to {} — a variant is being drawn as its base mob",
                model.name
            );
            assert_ne!(model.name, wrong);
            // ...and it must not merely resolve: the *sheet* has to differ too,
            // or the mob is still a zombie with a zombie skin under a new name.
            assert_ne!(
                entity_texture_candidates(ty),
                entity_texture_candidates(wrong),
                "{ty} shares {wrong}'s texture candidates"
            );
        }
    }

    /// The island this agent closed: `model_for_type` returned `None` for six
    /// of the seven `minecraft:*_minecart` registry types (every one except
    /// the plain `minecart`), so a chest/furnace/tnt/hopper minecart streamed
    /// correctly and drew nothing — `resolve_animated` silently skips any
    /// `type_path` with no baked model. All four now share the plain cart's
    /// `"minecart"` rig, matching vanilla's own abstract minecart renderer reusing
    /// one minecart model class for every subclass.
    ///
    /// All six subclasses, including `spawner_minecart` and
    /// `command_block_minecart`. Those two used to be a negative control here,
    /// asserted to resolve `None` on the grounds that this repo's own server
    /// never spawns them — a claim about the *server* standing in for a claim
    /// about the *client*, which meets them on anyone else's. The control that
    /// replaces it is the one that was always the real risk: a *non*-minecart
    /// type must not pick up the rig by name.
    #[test]
    fn minecart_subclasses_share_the_plain_carts_frame() {
        for entity_type in [
            EntityType::Minecart,
            EntityType::ChestMinecart,
            EntityType::FurnaceMinecart,
            EntityType::TntMinecart,
            EntityType::HopperMinecart,
            EntityType::SpawnerMinecart,
            EntityType::CommandBlockMinecart,
        ] {
            let ty = entity_type.path();
            let model = model_for_type(entity_type)
                .unwrap_or_else(|| panic!("{ty} must resolve to the minecart corpus rig"));
            assert_eq!(
                model.name, "minecart",
                "{ty} resolved to {} instead of the shared minecart rig",
                model.name
            );
        }
        // The control the roster above needs: the alias is an enum match, so a
        // type whose *path* merely ends in `minecart` must not reach the rig,
        // and a real mob must not either. `minecart` is not a suffix rule and
        // this is what says so.
        for absent in ["boat_minecart", "minecart_of_holding", "pig", "chest"] {
            assert!(
                canonical_model_name(absent) != Some("minecart"),
                "`{absent}` resolved to the minecart rig — the alias is an enum match on \
                 six real registry types, not a name rule"
            );
        }
    }

    /// The round trip from `u8` (the wire's
    /// registry id) → [`EntityType`] → [`model_for_type`], swept for all 158
    /// generated variants and checked against the independent `&str`-keyed
    /// path ([`canonical_model_name`], reached via
    /// [`EntityType::from_name`] rather than [`model_for_type`]) rather than
    /// against itself — the two implementations cannot silently disagree
    /// without this test naming which entity they disagreed on. Mismatches
    /// are collected, not asserted inside the loop, so a regression reports
    /// every disagreeing type instead of only the first.
    #[test]
    fn model_for_type_agrees_with_the_string_path_for_every_generated_entity_type() {
        let mut checked = 0usize;
        let mut mismatches = Vec::new();
        for entity_type in EntityType::all() {
            checked += 1;
            // `u8 -> EntityType`: the decode seam's only fallible step.
            let via_wire_id = EntityType::from_registry_id(entity_type.registry_id());
            if via_wire_id != Some(entity_type) {
                mismatches.push(format!(
                    "{}: registry id {} did not round-trip (got {via_wire_id:?})",
                    entity_type.path(),
                    entity_type.registry_id()
                ));
                continue;
            }
            let via_type = model_for_type(entity_type).map(|e| e.name);
            let via_str = canonical_model_name(entity_type.path());
            if via_type != via_str {
                mismatches.push(format!(
                    "{}: model_for_type={via_type:?} canonical_model_name(str)={via_str:?}",
                    entity_type.path()
                ));
            }
        }
        assert_eq!(
            checked,
            EntityType::COUNT as usize,
            "swept {checked} of {} generated variants — roster too small to be a real gate",
            EntityType::COUNT
        );
        assert!(
            mismatches.is_empty(),
            "{} of {checked} entity types disagree between the `EntityType` path and the \
             `&str` path:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    /// The sweep above could pass by accident if every code path happened to
    /// agree on some entity whose registry id and alphabetical rank coincide
    /// (registration order is not alphabetical, but nothing rules out a lone
    /// coincidence). This picks a **specific** entity where the two orders
    /// provably disagree and checks the round trip on exactly that one, so a
    /// hypothetical bug that resolves by alphabetical position instead of by
    /// registry id has a concrete input it cannot pass.
    #[test]
    fn model_for_type_round_trips_on_an_entity_whose_registry_id_and_alphabetical_rank_differ() {
        let mut alphabetical: Vec<EntityType> = EntityType::all().collect();
        alphabetical.sort_by_key(|e| e.path());
        let (alpha_rank, entity_type) = alphabetical
            .iter()
            .copied()
            .enumerate()
            .find(|&(alpha_rank, entity_type)| alpha_rank as u8 != entity_type.registry_id())
            .expect(
                "registration order and alphabetical order must differ somewhere among 158 \
                 entries — if this ever fires, the roster has no discriminating input left",
            );
        assert_ne!(
            alpha_rank as u8,
            entity_type.registry_id(),
            "chose a coincidentally-aligned entity, which cannot discriminate"
        );

        let via_wire_id =
            EntityType::from_registry_id(entity_type.registry_id()).expect("id round trip");
        assert_eq!(via_wire_id, entity_type);
        assert_eq!(
            model_for_type(via_wire_id).map(|e| e.name),
            canonical_model_name(entity_type.path()),
            "{} (registry id {}, alphabetical rank {alpha_rank}) disagreed",
            entity_type.path(),
            entity_type.registry_id()
        );
    }

    /// The suffix rules must not shadow the corpus's own names.
    ///
    /// `chest_boat` and `chest_raft` are corpus entries that also satisfy the
    /// `_boat`/`_raft` suffix tests, so a resolver that consults the suffixes
    /// *before* the corpus resolves the literal `"chest_boat"` to the plain
    /// `boat` rig — a silent wrong-mesh substitution for any caller that
    /// passes a corpus name straight through, which the `player_wide`/
    /// `player_slim` path already does.
    ///
    /// Moved from `tests/boat_model_resolution.rs`:
    /// none of `"boat"`/`"chest_boat"`/`"raft"`/`"chest_raft"` is a real
    /// `minecraft:entity_type` registry entry, so there is no `EntityType`
    /// value to hand `model_for_type` any more — this is squarely the
    /// surviving `&str`/corpus-name boundary, `canonical_model_name`, which
    /// is private to this module and so cannot be reached from an external
    /// integration test.
    #[test]
    fn a_literal_corpus_rig_name_still_resolves_to_itself() {
        for name in ["boat", "chest_boat", "raft", "chest_raft"] {
            assert_eq!(
                canonical_model_name(name),
                Some(name),
                "a literal corpus rig name must resolve to itself, not through the \
                 boat suffix rules"
            );
        }
    }

    /// The negative control: the suffix rules must not hand a boat rig to
    /// something that merely shares a word.
    ///
    /// `chest_minecart` has no ported rig (the corpus has `minecart` only),
    /// and a resolver matching on `contains("boat")`/`contains("chest")`
    /// rather than on the suffix would be caught here. Without this arm,
    /// "return `chest_boat` for anything with `chest` in it" would satisfy
    /// every assertion above. `"chest"`/`"boater"`/`"raft_of_ducks"` are not
    /// real registry paths at all, so — like the test above — this exercises
    /// `canonical_model_name` directly rather than `model_for_type`.
    ///
    /// Moved from `tests/boat_model_resolution.rs` for the same reason as
    /// [`a_literal_corpus_rig_name_still_resolves_to_itself`].
    #[test]
    fn a_non_boat_type_gets_no_boat_rig() {
        for name in ["chest_minecart", "chest", "boater", "raft_of_ducks", "pig"] {
            let resolved = canonical_model_name(name);
            assert!(
                !matches!(resolved, Some("boat" | "chest_boat" | "raft" | "chest_raft")),
                "`{name}` is not a boat, but resolved to {resolved:?}"
            );
        }
    }

    /// Control: `canonical_model_name` and `EntityModelSet::get`
    /// are a `OnceLock`-cached `HashSet`/`HashMap` index rather than an O(90)
    /// linear `&str` scan. This re-derives the *old* linear scan from
    /// scratch, independently of both functions under test, and checks it
    /// against the new implementation for every one of the 158 generated
    /// entity-type paths plus the non-registry pseudo-types
    /// (`player_wide`/`player_slim`, the four boat-family aliases) that
    /// `canonical_model_name` also has to resolve — the "world-species" gate,
    /// so a roster of only-already-corpus-named types
    /// cannot pass by never exercising the alias table.
    #[test]
    fn canonical_model_name_and_get_agree_with_an_independent_linear_scan() {
        // The alias arms are restated here rather than shared, which is the whole
        // point: this function is the pre-index implementation, re-derived. That
        // costs one line whenever a *genuine* alias lands — a real one has to be
        // mirrored here, and the mirror going red is the reminder — but it is the
        // only way the comparison is between two constructions rather than one.
        // What it is a control for is the **lookup structure** (linear scan vs
        // `OnceLock` index), not the alias set.
        fn old_canonical_model_name(type_path: &str) -> Option<&'static str> {
            match type_path {
                "player" | "mannequin" => return Some("player_wide"),
                "bogged" => return Some("skeleton"),
                "breeze_wind_charge" => return Some("wind_charge"),
                "chest_minecart"
                | "furnace_minecart"
                | "tnt_minecart"
                | "hopper_minecart"
                | "spawner_minecart"
                | "command_block_minecart" => {
                    return Some("minecart");
                }
                _ => {}
            }
            entity_models()
                .into_iter()
                .map(|e| e.name)
                .find(|n| *n == type_path)
                .or_else(|| boat_model_name(type_path))
        }

        let set = EntityModelSet::load();
        let mut checked = 0usize;
        let mut mismatches = Vec::new();
        let paths: Vec<&str> = (0..lodestone_data::entity_types::TYPE_COUNT as i32)
            .filter_map(lodestone_data::entity_types::entity_type_name)
            .map(|name| name.strip_prefix("minecraft:").unwrap())
            .collect();
        // Sanity: the roster really is the full 158, not an accidentally-empty
        // iterator that would make this test vacuous.
        assert_eq!(paths.len(), lodestone_data::entity_types::TYPE_COUNT as usize);

        for &type_path in paths
            .iter()
            .chain(["player_wide", "player_slim", "oak_boat", "oak_chest_boat", "bamboo_raft", "bamboo_chest_raft"].iter())
        {
            checked += 1;
            let old = old_canonical_model_name(type_path);
            let new = canonical_model_name(type_path);
            if old != new {
                mismatches.push(format!("{type_path}: old={old:?} new={new:?}"));
                continue;
            }
            // And the second scan `EntityModelSet::get` replaced, keyed by
            // whatever name `canonical_model_name` resolved to.
            if let Some(name) = new {
                let via_index = set.get(name);
                let via_scan = set.models.iter().find(|(n, _)| *n == name).map(|(_, m)| m);
                if !std::ptr::eq(
                    via_index.map_or(std::ptr::null(), |m| m as *const _),
                    via_scan.map_or(std::ptr::null(), |m| m as *const _),
                ) {
                    mismatches.push(format!("{name}: EntityModelSet::get index/scan disagree"));
                }
            }
        }
        assert!(checked >= 158 + 6, "roster too small to be a real gate: {checked}");
        assert!(
            mismatches.is_empty(),
            "{} of {checked} paths disagree between the old linear scan and the new \
             indexed lookup:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    #[test]
    fn unknown_entity_type_has_no_model() {
        // Types the corpus genuinely has no mesh for — the renderer skips them
        // rather than substituting something mob-shaped.
        //
        // `arrow` used to be the headline entry here: the physics was
        // modelled in `lodestone-entity`, no rig existed, and this assert was the
        // written record of that gap. It is kept as its **positive** form rather
        // than deleted, so the gap closing is visible in the diff of the test that
        // recorded it — and so a corpus edit that silently dropped the rig fails
        // here rather than only in an `#[ignore]`d pixel gate.
        //
        // **`experience_orb` is a different case from the other two, and it is
        // *not* a pinned absence waiting to be inverted.** An orb now draws — see
        // `experience_orb_mesh`/`experience_orb_matrix` above — as a camera-facing
        // sprite, which is not a cuboid part hierarchy and so is not a corpus
        // entry. Adding one would make `EntityModelSet::resolve` hand the mob pass
        // a rig for an entity that has none, which is a worse failure than the
        // nothing that used to draw. The precedent is `ITEM_ENTITY_TYPE_PATH`: a
        // dropped item reaches the render path and is deliberately absent here for
        // exactly the same reason. So this assertion is load-bearing *after* the
        // orb landed, not before it.
        assert!(model_for_type(EntityType::ExperienceOrb).is_none());
        assert!(model_for_type(EntityType::Tnt).is_none());
        // An empty/garbage path is not something `model_for_type` can even be
        // asked any more — there is no `EntityType` value for it — so the
        // equivalent negative belongs to the surviving `&str` boundary,
        // `canonical_model_name`, which every non-registry type path (still)
        // routes through.
        assert!(canonical_model_name("").is_none());
    }

    /// The other side of [`unknown_entity_type_has_no_model`]: `arrow`,
    /// `spectral_arrow`, and `trident` resolve, and resolve to their **own**
    /// rigs.
    ///
    /// `arrow` and `spectral_arrow` deliberately *share* a builder
    /// (vanilla's arrow renderer bakes one shared arrow model layer for both), so equal geometry
    /// is correct there and the sheet is the only thing that must differ — the
    /// same drowned-vs-zombie shape as `variant_mobs_point_at_their_own_sheet`.
    /// `trident` is a genuine sibling with its own mesh, so its geometry must
    /// differ too.
    #[test]
    fn projectiles_resolve_to_their_own_rigs_and_sheets() {
        for entity_type in [EntityType::Arrow, EntityType::SpectralArrow, EntityType::Trident] {
            let ty = entity_type.path();
            let model =
                model_for_type(entity_type).unwrap_or_else(|| panic!("{ty} must have a corpus model"));
            assert_eq!(model.name, ty);
            assert_eq!(
                entity_texture_candidates(ty).len(),
                1,
                "{ty} should have exactly one sheet (no `_temperate` legacy fallback)"
            );
        }
        assert_eq!(
            entity_texture_candidates("arrow"),
            ["assets/minecraft/textures/entity/projectiles/arrow.png"]
        );
        assert_eq!(
            entity_texture_candidates("spectral_arrow"),
            ["assets/minecraft/textures/entity/projectiles/arrow_spectral.png"]
        );
        assert_eq!(
            entity_texture_candidates("trident"),
            ["assets/minecraft/textures/entity/trident/trident.png"]
        );
        // Same rig, different sheet for the two arrows; a different rig entirely
        // for the trident.
        let set = EntityModelSet::load();
        let arrow = set.get("arrow").expect("arrow mesh");
        let spectral = set.get("spectral_arrow").expect("spectral_arrow mesh");
        let trident = set.get("trident").expect("trident mesh");
        assert_eq!(arrow.vertices.len(), spectral.vertices.len());
        assert_ne!(
            arrow.vertices.len(),
            trident.vertices.len(),
            "trident must not be sharing the arrow rig"
        );
    }

    /// Every projectile in the corpus must be on the projectile placement, and
    /// **no mob may be**. The switch is one `match`; getting an entry wrong in
    /// either direction is silent — a mob on the projectile path loses its
    /// 1.501-block lift, an arrow on the mob path gains one.
    #[test]
    fn exactly_the_projectile_models_take_the_projectile_placement() {
        let mut projectiles = Vec::new();
        for entry in entity_models() {
            if projectile_pitch_offset_deg(entry.name).is_some() {
                projectiles.push(entry.name);
            }
        }
        // Corpus order, not alphabetical — this is the sweep's own iteration
        // order, so a rig moving in the corpus is visible here too.
        assert_eq!(
            projectiles,
            ["llama_spit", "arrow", "spectral_arrow", "trident"]
        );
        // A spot-check of the negative direction that names real mobs rather than
        // relying on the sweep above: these are the two families whose renderer is
        // most often assumed to be a generic entity renderer.
        for mob in ["pig", "player_wide", "zombie", "boat", "end_crystal"] {
            assert!(
                projectile_pitch_offset_deg(mob).is_none(),
                "{mob} must stay on vanilla's living-entity renderer placement"
            );
        }
    }

    /// The placement itself, against hand-derived values rather than against
    /// [`projectile_model_matrix`]'s own output.
    ///
    /// The three things that would each be individually plausible and wrong:
    /// a `MODEL_FEET_OFFSET` lift, a mirror, and pitch about `X` instead of `Z`.
    #[test]
    fn projectile_placement_has_no_lift_no_mirror_and_pitches_about_z() {
        let pos = Vec3::new(3.0, 64.0, -7.0);
        let m = projectile_model_matrix(pos, 0.0, 0.0, 1.0);
        // (a) No lift: the model origin lands exactly on the reported position.
        let origin = m.transform_point3(Vec3::ZERO);
        assert!(
            (origin - pos).length() < 1e-5,
            "projectile origin {origin} is not the entity position {pos} — a \
             MODEL_FEET_OFFSET lift has crept in"
        );
        // (b) No mirror: determinant of the linear part is positive. The mob
        // matrix's `scale(-1,-1,1)` is +1 too (two flips cancel), so this is not
        // the discriminator for the flip — `arrow_pixels` is. It does catch a
        // single-axis mirror.
        let det = glam::Mat3::from_mat4(m).determinant();
        assert!(det > 0.0, "determinant {det} — geometry is mirrored");

        // (c) Pitch is about Z. At yaw 0 the shaft (model +X) must point +Z; at
        // pitch +45° it must rise. Hand-derived: Ry(-90) maps +X to +Z, and Rz(45)
        // first sends +X to (cos45, sin45, 0), so the tip ends at
        // (0, sin45, cos45) — i.e. equal parts up and forward, with **zero** x.
        let tip = |pitch: f32| {
            projectile_model_matrix(Vec3::ZERO, 0.0, pitch, 1.0)
                .transform_point3(Vec3::new(1.0, 0.0, 0.0))
        };
        let level = tip(0.0);
        assert!(
            (level - Vec3::new(0.0, 0.0, 1.0)).length() < 1e-5,
            "at yaw 0 / pitch 0 the shaft points {level}, not +Z"
        );
        let up = tip(45.0);
        let root_half = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (up - Vec3::new(0.0, root_half, root_half)).length() < 1e-5,
            "at pitch 45 the shaft points {up}, not (0, √½, √½) — a rotation about \
             X instead of Z spins the arrow about its own axis and leaves this at +Z"
        );

        // (d) Yaw agrees with the *projectile* convention, which is not the mob
        // one. Vanilla's projectile-shoot function sets yRot = atan2(mx, mz), so the shaft must
        // point along (sin yaw, 0, cos yaw) — note the **+** sin, where a mob's
        // facing is (-sin yaw, 0, cos yaw).
        for yaw in [0.0f32, 37.0, 90.0, 180.0, -125.0] {
            let dir = projectile_model_matrix(Vec3::ZERO, yaw, 0.0, 1.0)
                .transform_point3(Vec3::new(1.0, 0.0, 0.0));
            let want = Vec3::new(
                yaw.to_radians().sin(),
                0.0,
                yaw.to_radians().cos(),
            );
            assert!(
                (dir - want).length() < 1e-5,
                "yaw {yaw}: shaft points {dir}, want {want}"
            );
        }

        // (e) The trident's +90° offset is what puts its own long axis (model -Y)
        // where the arrow's +X is: both must point the same way for the same
        // reported rotation.
        let arrow_dir = projectile_model_matrix(Vec3::ZERO, 20.0, 15.0, 1.0)
            .transform_point3(Vec3::new(1.0, 0.0, 0.0));
        let trident_dir = projectile_model_matrix(
            Vec3::ZERO,
            20.0,
            15.0 + projectile_pitch_offset_deg("trident").expect("trident is a projectile"),
            1.0,
        )
        .transform_point3(Vec3::new(0.0, -1.0, 0.0));
        assert!(
            (arrow_dir.normalize() - trident_dir.normalize()).length() < 1e-5,
            "trident tip {trident_dir} does not point where the arrow tip {arrow_dir} does"
        );
    }

    /// The whole point of the separate placement, stated as a delta a reviewer can
    /// check by eye: a projectile and a mob at the *same* reported position put
    /// their **model origin** [`MODEL_FEET_OFFSET`] apart in Y, and the arrow's
    /// tip in the opposite direction along X.
    ///
    /// # The sign is the other way round from the obvious guess
    ///
    /// This test's own first draft assumed
    /// reusing the mob matrix would draw an arrow "1.5 blocks **low**". It draws
    /// it 1.5 blocks **high**, and the difference is the mirror, not the lift:
    /// `entity_model_matrix` is `T(feet) · Ry · S(-1,-1,1) · T(0, -1.501, 0)`, so
    /// the lift is applied *before* the Y negation and comes back out as
    /// `feet + 1.501`. That is exactly right for a mob — model space is Y-down and
    /// the model origin is a humanoid's shoulder line, ~1.5 blocks up — and
    /// exactly wrong for a rig authored the other way up. The first draft asserted
    /// `feet - 1.501` and failed at `65.501`; the control's premise was false in
    /// the safe-looking direction, which is why it is spelled out here rather than
    /// quietly corrected.
    #[test]
    fn reusing_the_mob_matrix_would_lift_an_arrow_and_reverse_it() {
        let pos = Vec3::new(0.0, 64.0, 0.0);
        let projectile = projectile_model_matrix(pos, 0.0, 0.0, 1.0).transform_point3(Vec3::ZERO);
        let mob = entity_model_matrix(pos, 0.0, 1.0).transform_point3(Vec3::ZERO);
        assert!(
            (projectile.y - pos.y).abs() < 1e-5,
            "the projectile placement moved the model origin off the reported \
             position: {} vs {}",
            projectile.y,
            pos.y
        );
        assert!(
            (mob.y - (pos.y + MODEL_FEET_OFFSET)).abs() < 1e-5,
            "mob model origin at {} — expected feet + {MODEL_FEET_OFFSET}. If this \
             fires, the control for this test is wrong, not the code under test",
            mob.y
        );
        assert!(
            mob.y - projectile.y > 1.5,
            "the two placements differ by only {} blocks in Y",
            mob.y - projectile.y
        );

        // The second half of the damage, which the Y offset alone would hide: the
        // two placements send the arrow's tip (model `+X`) different ways.
        //
        // Hand-derived. The projectile linear part is `Ry(yaw - 90)`, which sends
        // `+X` to `(sin yaw, 0, cos yaw)` — the motion direction. The mob linear
        // part is `Ry(180 - yaw) · S(-1, -1, 1)`, which sends it to
        // `(cos yaw, 0, sin yaw)`. Those are **reflections of each other across the
        // `x = z` diagonal**, not a fixed rotation apart — so they happen to agree
        // at `yaw = 45°` and are exactly opposed at `135°`. Asserting "the two
        // point opposite ways" at an arbitrary yaw is therefore a control whose
        // premise is false a quarter of the time; assert the relation instead, and
        // then name the yaw where it is worst.
        for yaw in [0.0f32, 90.0, 135.0, -20.0] {
            let (s, c) = (yaw.to_radians().sin(), yaw.to_radians().cos());
            let tip = Vec3::new(1.0, 0.0, 0.0);
            let good = projectile_model_matrix(Vec3::ZERO, yaw, 0.0, 1.0).transform_point3(tip);
            let m = entity_model_matrix(Vec3::ZERO, yaw, 1.0);
            let bad = m.transform_point3(tip) - m.transform_point3(Vec3::ZERO);
            assert!(
                (good - Vec3::new(s, 0.0, c)).length() < 1e-5,
                "yaw {yaw}: projectile tip {good}, want (sin, 0, cos)"
            );
            assert!(
                (bad - Vec3::new(c, 0.0, s)).length() < 1e-5,
                "yaw {yaw}: mob-placed tip {bad}, want (cos, 0, sin)"
            );
        }
        // The worst case, spelled out: at 135° the mob placement flies the arrow
        // exactly backwards.
        let good = projectile_model_matrix(Vec3::ZERO, 135.0, 0.0, 1.0)
            .transform_point3(Vec3::new(1.0, 0.0, 0.0));
        let m = entity_model_matrix(Vec3::ZERO, 135.0, 1.0);
        let bad =
            m.transform_point3(Vec3::new(1.0, 0.0, 0.0)) - m.transform_point3(Vec3::ZERO);
        assert!(
            good.normalize().dot(bad.normalize()) < -0.99,
            "at yaw 135 the placements should be opposed: {good} vs {bad}"
        );
    }

    #[test]
    fn every_drawable_model_has_a_texture_candidate() {
        // Now that the drawable set *is* the corpus, sweep the whole corpus:
        // every baked model gets uploaded with a sheet by the shell, so a model
        // with no candidate is a mob that draws as a flat placeholder colour.
        let mut checked = 0;
        for entry in entity_models() {
            let candidates = entity_texture_candidates(entry.name);
            assert!(
                !candidates.is_empty(),
                "model {:?} has no texture candidate",
                entry.name
            );
            for path in candidates {
                assert!(
                    path.starts_with("assets/minecraft/textures/entity/") && path.ends_with(".png"),
                    "candidate {path:?} for {:?} is not an entity sheet path",
                    entry.name
                );
            }
            checked += 1;
        }
        assert!(checked > 60, "only {checked} models swept");
        // The temperature-variant mobs keep their pre-26.2 sheet as a fallback,
        // so one binary works against both pack layouts.
        assert_eq!(
            entity_texture_candidates("pig"),
            [
                "assets/minecraft/textures/entity/pig/pig_temperate.png",
                "assets/minecraft/textures/entity/pig/pig.png",
            ]
        );
        // A name that is not a model resolves to nothing rather than a wrong sheet.
        // This was `"arrow"` until the arrow rig landed; the
        // assertion is kept (with a name that really is not a corpus entry) rather
        // than deleted, because "an unknown name yields no sheet" is the property
        // that stops a typo in the corpus from silently drawing a mob under some
        // other mob's skin. `arrow`'s own sheet is asserted positively in
        // `projectiles_resolve_to_their_own_rigs_and_sheets`.
        // `experience_orb` stays empty **after** the orb started drawing, and that
        // is not an oversight: this function answers "which sheet does a *corpus
        // rig* wear", and the orb has no rig. Its sheet is
        // [`EXPERIENCE_ORB_TEXTURE`], bound by the orb pass's own group 1 the same
        // way the mob-fire strip is — neither goes through this table.
        assert!(entity_texture_candidates("experience_orb").is_empty());
        assert!(entity_texture_candidates("").is_empty());
    }

    /// [`special_item_hover_lift`] must measure **all eight corners** of the rig's
    /// box through the display transform, not just `local_min`.
    ///
    /// The wrong hypothesis — transform `local_min` alone — agrees exactly whenever
    /// the transform has no rotation, which is true of most `ground` transforms. So
    /// the discriminating input is a transform that *does* rotate: a 90° turn about
    /// `x` sends the box's lowest posed point to the image of a **different**
    /// corner, and the two answers then differ by the box's own depth.
    ///
    /// Both hypotheses are computed here from the same box, and the test fails if
    /// they coincide at the chosen input.
    #[test]
    fn the_hover_lift_measures_the_whole_box_not_just_its_lowest_corner() {
        use lodestone_assets::DisplayTransform;
        // A tall, shallow box, so a rotation about `x` visibly changes which corner
        // is lowest: y spans 0.75 and z spans 0.25.
        let local_min = Vec3::new(-0.5, 0.0, -0.125);
        let local_max = Vec3::new(0.5, 0.75, 0.125);

        // No rotation, so the two hypotheses must agree. The expected value is
        // **not** `ITEM_MIN_HOVER_HEIGHT` alone, and that is the whole reason to
        // state it: `display_matrix` ends with vanilla's own
        // `translate(-0.5, -0.5, -0.5)` (vanilla's item-transform apply function, taken even by
        // `NO_TRANSFORM`), which centres a `[0,1]³` model. A box whose bottom is at
        // local `y = 0` therefore poses at `y = -0.5`, and the lift is
        // `0.5 + ITEM_MIN_HOVER_HEIGHT`. Predicting the round `0.0625` here is the
        // mistake, and it fails in the direction that looks like a code bug.
        let flat = DisplayTransform::default();
        let lift = special_item_hover_lift(local_min, local_max, &flat);
        let expected = 0.5 + ITEM_MIN_HOVER_HEIGHT;
        assert!(
            (lift - expected).abs() < 1.0e-5,
            "an unrotated box whose bottom is at local y=0 poses at y=-0.5 through \
             the display centring, so the lift is {expected}; got {lift}"
        );

        // A 90° turn about `x`. `DisplayTransform`'s rotation is in degrees.
        let turned = DisplayTransform {
            rotation: [90.0, 0.0, 0.0],
            ..DisplayTransform::default()
        };
        let got = special_item_hover_lift(local_min, local_max, &turned);
        // The wrong hypothesis, evaluated at this input: `local_min` alone.
        let naive = -display_matrix(&turned).transform_point3(local_min).y + ITEM_MIN_HOVER_HEIGHT;
        assert!(
            (got - naive).abs() > 1.0e-3,
            "at this input the whole-box lift ({got}) and the lowest-corner lift \
             ({naive}) coincide, so this test measures nothing"
        );
        // And the answer is the real one: the lowest posed corner must land exactly
        // `ITEM_MIN_HOVER_HEIGHT` above zero once the lift is applied.
        let pose = display_matrix(&turned);
        let mut lowest = f32::INFINITY;
        for i in 0..8u8 {
            let corner = Vec3::new(
                if i & 1 == 0 { local_min.x } else { local_max.x },
                if i & 2 == 0 { local_min.y } else { local_max.y },
                if i & 4 == 0 { local_min.z } else { local_max.z },
            );
            lowest = lowest.min(pose.transform_point3(corner).y);
        }
        assert!(
            (lowest + got - ITEM_MIN_HOVER_HEIGHT).abs() < 1.0e-5,
            "lifted bottom lands at {}, expected {ITEM_MIN_HOVER_HEIGHT}",
            lowest + got
        );
    }

    /// A framed item sits just outside the attachment block's wall face, on the
    /// side the frame faces, and is drawn at vanilla's `0.5` rather than the
    /// framed *map*'s full block.
    ///
    /// # The magnitudes, not the signs
    ///
    /// A sign-only assertion ("it lifts toward `+z`") is satisfied by both
    /// candidate readings of this chain, because both put a south-facing frame's
    /// item somewhere in `+z`. The two disagree about *how far*, and that is the
    /// whole bug:
    ///
    /// | reading | offset from the attachment block centre |
    /// |---|---|
    /// | lift along the frame's own `+z`, which points **into** the wall | `-0.4375 · facing` — correct |
    /// | lift along the facing, i.e. the signs added | `+0.4375 · facing` — through the block |
    ///
    /// So each arm predicts the correct value *and* names the wrong one, per
    /// `CLAUDE.md`'s magnitude rule.
    ///
    /// The scale carries the same treatment: `prepare_framed_maps` draws its
    /// picture at `1.0`, and copying that number is the plausible mistake — it
    /// draws, it faces the right way, and it is twice the size of the frame
    /// around it.
    #[test]
    fn a_framed_item_sits_just_in_front_of_its_frame_and_is_drawn_half_size() {
        use lodestone_assets::DisplayTransform;
        let identity = DisplayTransform::default();
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let block_centre = anchor + Vec3::splat(0.5);
        // A block-entity rig lives in the block's own `[0,1]³` corner-origin space
        // (see `block_entity_placement_matrix`'s pivot of `(0.5, 0, 0.5)`), and
        // `display_matrix`'s trailing `translate(-0.5, -0.5, -0.5)` centres exactly
        // that space. So the point that maps to the pose origin is the rig's
        // **centre**, not its corner — probing `Vec3::ZERO` measures a corner and
        // reports a spurious offset along every axis the rotation touches.
        let centre_of = |m: Mat4| m.transform_point3(Vec3::splat(0.5));
        const CONTENT_LIFT: f32 = 0.4375;

        // Yaw 0 is south (`+z`) in vanilla.
        let pose = framed_item_matrix(anchor, 0.0, 0.0, 0, false, &identity);
        let centre = centre_of(pose);
        let south_expected = block_centre - Vec3::new(0.0, 0.0, CONTENT_LIFT);
        assert!(
            (centre - south_expected).length() < 1.0e-5,
            "a south-facing frame's item must sit at {south_expected}, got {centre}"
        );
        let north = centre_of(framed_item_matrix(anchor, 180.0, 0.0, 0, false, &identity));
        assert!(
            (north - (block_centre + Vec3::new(0.0, 0.0, CONTENT_LIFT))).length() < 1.0e-5,
            "a north-facing frame's item must mirror about the attachment block centre, got {north}"
        );
        // West and east are the arms that separate the frame's real `Direction`
        // from a bare `Ry(yaw)`: those two agree at yaw 0 and 180 and disagree in
        // sign at 90 and 270, so a corpus that only ever probes a north/south wall
        // cannot see the difference at all.
        let west = centre_of(framed_item_matrix(anchor, 90.0, 0.0, 0, false, &identity));
        assert!(
            (west - (block_centre + Vec3::new(CONTENT_LIFT, 0.0, 0.0))).length() < 1.0e-5,
            "yaw 90 is west, so the item must move to +x from the attachment centre; got {west}"
        );
        let east = centre_of(framed_item_matrix(anchor, 270.0, 0.0, 0, false, &identity));
        assert!(
            (east - (block_centre - Vec3::new(CONTENT_LIFT, 0.0, 0.0))).length() < 1.0e-5,
            "yaw 270 is east, so the item must move to -x from the attachment centre; got {east}"
        );

        // The scale: the rig's full `[0,1]` width must come out half a block.
        let left = pose.transform_point3(Vec3::new(0.0, 0.5, 0.5));
        let right = pose.transform_point3(Vec3::new(1.0, 0.5, 0.5));
        let width = (right - left).length();
        assert!(
            (width - 0.5).abs() < 1.0e-5,
            "a unit-wide rig must draw 0.5 blocks wide, got {width}"
        );
        assert!(
            (width - 1.0).abs() > 0.1,
            "the framed-map scale of 1.0 would also pass the facing assertions above"
        );

        // Pitch 90 is a **ceiling** frame, not a floor one: vanilla's frame
        // direction-setter
        // writes `xRot = -90 * direction.getAxisDirection().getStep()`, and
        // the down direction's step is `-1`. So a pitch-90 frame faces down and its
        // item hangs *below* the entity. Reading it the other way round is the
        // mistake this arm exists to name, and it is invisible on a wall frame.
        let ceiling = centre_of(framed_item_matrix(anchor, 0.0, 90.0, 0, false, &identity));
        assert!(
            (ceiling - (block_centre + Vec3::new(0.0, CONTENT_LIFT, 0.0))).length() < 1.0e-5,
            "a pitch-90 frame faces DOWN, so its item must sit above the attachment centre; got {ceiling}"
        );
        let floor = centre_of(framed_item_matrix(anchor, 0.0, -90.0, 0, false, &identity));
        assert!(
            (floor - (block_centre - Vec3::new(0.0, CONTENT_LIFT, 0.0))).length() < 1.0e-5,
            "a pitch -90 frame faces UP, so its item must sit below the attachment centre; got {floor}"
        );
        assert!(
            (ceiling.z - block_centre.z).abs() < 1.0e-4,
            "a pitched frame must not also move along z, got z={}",
            ceiling.z
        );
    }

    /// The frame's **body** covers the wall face of the block it hangs in, and its
    /// back plate is against that wall rather than facing the room.
    ///
    /// The discriminating quantity is the back plate's `z`. `template_item_frame`'s
    /// back element spans `z = 15.5..16` in model units — local `0.96875..1.0` —
    /// so under [`item_frame_body_matrix`] it must land in the 1/32 of a block
    /// **behind** the block's centre plane, on the wall side. Dropping the
    /// `180 - yaw` mirrors it to the front, which still looks like a frame from a
    /// distance and hides the item behind its own backing.
    #[test]
    fn the_item_frame_body_puts_its_back_plate_against_the_wall() {
        // The actual spawn packet carries this attachment block position.
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let centre = anchor + Vec3::splat(0.5);
        let pose = item_frame_body_matrix(anchor, 0.0, 0.0);

        // Local (0.5, 0.5, 1.0) is the middle of the back plate's outer face.
        let back = pose.transform_point3(Vec3::new(0.5, 0.5, 1.0));
        assert!(
            (back - (centre - Vec3::new(0.0, 0.0, 0.5))).length() < 1.0e-5,
            "the back plate's outer face must land on the wall side of the cell \
             ({}), got {back}",
            centre - Vec3::new(0.0, 0.0, 0.5)
        );
        // And the opposite face is the one the room sees.
        let front = pose.transform_point3(Vec3::new(0.5, 0.5, 0.0));
        assert!(
            front.z > back.z,
            "the frame is drawn back-to-front: back={back}, front={front}"
        );

        // The body is a full block across, unrotated in its own plane: a unit-wide
        // model must measure one block, which is what separates it from the item's
        // own half scale.
        let left = pose.transform_point3(Vec3::new(0.0, 0.5, 1.0));
        let right = pose.transform_point3(Vec3::new(1.0, 0.5, 1.0));
        assert!(
            ((right - left).length() - 1.0).abs() < 1.0e-5,
            "the frame body draws at 1:1, got {}",
            (right - left).length()
        );
    }

    /// An item frame's add-entity packet carries its attachment `BlockPos`, not
    /// its offset entity centre. The renderer therefore begins at that block's
    /// centre after its dispatch/render-offset pair cancels.
    #[test]
    fn item_frame_space_centres_the_integer_packet_anchor() {
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let expected = anchor + Vec3::splat(0.5);
        for (yaw, pitch) in [
            (0.0_f32, 0.0_f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let origin = item_frame_space(anchor, yaw, pitch).transform_point3(Vec3::ZERO);
            assert!(
                (origin - expected).length() < 1.0e-5,
                "yaw {yaw}, pitch {pitch}: packet anchor {anchor} must centre frame space at {expected}, got {origin}"
            );
        }
    }

    /// The back of the one-block frame body is flush with the attachment block's
    /// wall face for every possible `Direction`.
    #[test]
    fn item_frame_body_back_plate_lands_on_the_packet_anchors_wall_face() {
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        for (yaw, pitch) in [
            (0.0_f32, 0.0_f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let facing = item_frame_facing_step(yaw, pitch);
            let back = item_frame_body_matrix(anchor, yaw, pitch)
                .transform_point3(Vec3::new(0.5, 0.5, 1.0));
            let expected = anchor + Vec3::splat(0.5) - facing * 0.5;
            assert!(
                (back - expected).length() < 1.0e-5,
                "yaw {yaw}, pitch {pitch}: body back must lie on attachment wall {expected}, got {back}"
            );
        }
    }

    #[test]
    fn item_frame_culling_box_matches_wall_offset_dimensions_and_renderer_inflate() {
        let anchor = Vec3::new(1965.0, 73.0, 3806.0);
        let (map_min, map_max) = item_frame_culling_aabb(anchor, 90.0, 0.0, true);
        assert!((map_min - Vec3::new(1965.4375, 72.5, 3805.5)).length() < 1.0e-5);
        assert!((map_max - Vec3::new(1966.5, 74.5, 3807.5)).length() < 1.0e-5);

        let (plain_min, plain_max) = item_frame_culling_aabb(anchor, 90.0, 0.0, false);
        assert!((plain_min - Vec3::new(1965.4375, 72.625, 3805.625)).length() < 1.0e-5);
        assert!((plain_max - Vec3::new(1966.5, 74.375, 3807.375)).length() < 1.0e-5);
    }

    /// [`item_frame_facing_step`] is the frame's real `Direction`, and the four
    /// horizontal yaws are the inputs that prove it.
    ///
    /// The wrong hypothesis is a plain `Ry(yaw)` applied to `+z` — the expression
    /// the framed-*map* path used to lift by. It agrees with the truth at yaw `0`
    /// and `180` and is the exact opposite at `90` and `270`, so any gate probing
    /// only a north or south wall passes under both.
    #[test]
    fn the_item_frame_facing_step_is_the_frames_own_direction() {
        for (yaw, expected) in [
            (0.0_f32, Vec3::new(0.0, 0.0, 1.0)),
            (90.0, Vec3::new(-1.0, 0.0, 0.0)),
            (180.0, Vec3::new(0.0, 0.0, -1.0)),
            (270.0, Vec3::new(1.0, 0.0, 0.0)),
        ] {
            let got = item_frame_facing_step(yaw, 0.0);
            assert!(
                (got - expected).length() < 1.0e-5,
                "yaw {yaw}: expected {expected}, got {got}"
            );
        }
        // Vanilla's down direction has its own axis-direction step == -1, so its
        // pitch is `+90` — a pitch-90 frame faces down.
        assert!(
            (item_frame_facing_step(0.0, 90.0) - Vec3::new(0.0, -1.0, 0.0)).length() < 1.0e-5,
            "pitch 90 must face DOWN, got {}",
            item_frame_facing_step(0.0, 90.0)
        );
        assert!(
            (item_frame_facing_step(0.0, -90.0) - Vec3::new(0.0, 1.0, 0.0)).length() < 1.0e-5,
            "pitch -90 must face UP, got {}",
            item_frame_facing_step(0.0, -90.0)
        );
    }

    /// The eight-step in-frame rotation turns the item about the frame's own
    /// normal, and a full eight steps is the identity.
    ///
    /// A quarter turn is the discriminating input: two steps of `45°` must move a
    /// point off the frame's centre line by the full radius, which a `rotation`
    /// that never reaches the matrix (the state before it was decoded) cannot do.
    #[test]
    fn the_framed_item_rotation_turns_about_the_frames_normal() {
        use lodestone_assets::DisplayTransform;
        let identity = DisplayTransform::default();
        let feet = Vec3::new(4.0, 65.0, -9.0);
        // A point one unit up the rig's own +y, so a rotation about z moves it.
        let probe = Vec3::new(0.5, 1.5, 0.5);
        let at = |rotation: u8| {
            framed_item_matrix(feet, 0.0, 0.0, rotation, false, &identity).transform_point3(probe)
        };

        let up = at(0);
        let quarter = at(2);
        let half = at(4);
        // Two steps = 90°: what was straight up is now straight sideways.
        assert!(
            (quarter.y - up.y).abs() > 0.4,
            "two rotation steps must move the probe off vertical: {up} -> {quarter}"
        );
        assert!(
            (half.y - up.y).abs() > 0.9,
            "four rotation steps must invert it: {up} -> {half}"
        );
        // Eight steps is a full turn, and `% 8` makes 8 and 0 the same pose.
        assert!(
            (at(8) - up).length() < 1.0e-5,
            "rotation 8 must equal rotation 0, got {} vs {up}",
            at(8)
        );
    }

    /// An invisible frame lifts its contents further, because there is no body to
    /// hold them clear of the wall.
    ///
    /// `0.5` against `0.4375` — a 1/16 difference, which is exactly the sort of
    /// magnitude a direction-only assertion cannot see.
    #[test]
    fn an_invisible_frame_lifts_its_contents_the_extra_sixteenth() {
        assert!((item_frame_content_lift(false) - 0.4375).abs() < 1.0e-6);
        assert!((item_frame_content_lift(true) - 0.5).abs() < 1.0e-6);
        // The consequence in world space: an invisible frame's item reaches the
        // attachment block wall; the visible item stays one sixteenth outside it.
        use lodestone_assets::DisplayTransform;
        let identity = DisplayTransform::default();
        let feet = Vec3::new(4.0, 65.0, -9.0);
        let centre_of = |m: Mat4| m.transform_point3(Vec3::splat(0.5));
        let visible = centre_of(framed_item_matrix(feet, 0.0, 0.0, 0, false, &identity));
        let invisible = centre_of(framed_item_matrix(feet, 0.0, 0.0, 0, true, &identity));
        assert!(
            ((visible.z - invisible.z) - 0.0625).abs() < 1.0e-5,
            "the two lifts must differ by exactly 1/16, got {} vs {}",
            visible.z,
            invisible.z
        );
    }

    /// The orb sprite cell is a **bucketed** lookup, and the only inputs that can
    /// observe that are ones straddling a threshold.
    ///
    /// The wrong hypothesis this discriminates against is a *linear* map from value
    /// to cell — the shape a reader would reach for, and the shape a single-value
    /// gate cannot rule out. Both hypotheses are evaluated at every input below and
    /// the test fails if they ever agree there, so the inputs cannot silently stop
    /// discriminating.
    #[test]
    fn orb_icon_is_bucketed_and_constant_inside_a_bucket() {
        // The linear reading someone would write instead: eleven cells spread
        // evenly over the top denomination.
        let linear = |value: i32| -> u32 {
            ((value.max(0) as u32) * (EXPERIENCE_ORB_ICON_COUNT - 1) / 2477)
                .min(EXPERIENCE_ORB_ICON_COUNT - 1)
        };
        // `(value, expected cell, must discriminate)`. Every pair is either side of
        // a threshold, so a version that shifted one boundary by one fails.
        //
        // The three `false` rows are the ladder's **endpoints**, where the two
        // hypotheses provably agree and no choice of input can separate them: any
        // monotone map from value to cell sends the bottom of the range to cell 0
        // and the top to cell 10. They are kept as correctness assertions (a
        // transcription that dropped the `>= 2477` arm still fails) and excluded
        // from the discrimination requirement rather than quietly satisfying it —
        // which is what the coincidence check below exists to force.
        let cases: [(i32, u32, bool); 14] = [
            (0, 0, false),
            (2, 0, false),
            (3, 1, true),
            (6, 1, true),
            (7, 2, true),
            (16, 2, true),
            (17, 3, true),
            (36, 3, true),
            (37, 4, true),
            (73, 5, true),
            (149, 6, true),
            (307, 7, true),
            (617, 8, true),
            (2477, 10, false),
        ];
        let mut mismatches = Vec::new();
        let mut coincidences = Vec::new();
        for (value, expected, discriminating) in cases {
            let got = experience_orb_icon(value);
            if got != expected {
                mismatches.push(format!("value {value}: expected cell {expected}, got {got}"));
            }
            // The corollary that makes this a test rather than a re-run of the
            // code: an input where the bucketed and the linear answer coincide
            // measures nothing.
            if discriminating && linear(value) == expected {
                coincidences.push(format!(
                    "value {value} cannot discriminate — the linear hypothesis also says {expected}"
                ));
            }
        }
        // And the reciprocal, so the eleven `true` rows cannot all quietly become
        // endpoint-like if someone rewrites `linear`: at least ten of them must
        // really separate the two hypotheses.
        let separating = cases
            .iter()
            .filter(|(value, expected, discriminating)| *discriminating && linear(*value) != *expected)
            .count();
        assert!(
            separating >= 10,
            "only {separating} inputs separate the bucketed and linear hypotheses"
        );
        assert!(mismatches.is_empty(), "{mismatches:#?}");
        assert!(coincidences.is_empty(), "{coincidences:#?}");
        // And the property a per-value table could satisfy while still being
        // linear: the cell must be *constant* across a whole bucket. 7..=16 is ten
        // consecutive values that all draw cell 2.
        let inside: Vec<u32> = (7..=16).map(experience_orb_icon).collect();
        assert_eq!(inside, vec![2; 10], "cell 2's bucket is not flat: {inside:?}");
    }

    /// Two cells of the sheet must address two *different* 16-pixel squares of the
    /// 64-pixel sheet, and the row must advance every four cells.
    ///
    /// The failure this rules out is the one a "does it draw?" check cannot see: a
    /// mesh that always samples cell 0 draws a perfectly plausible orb for every
    /// value in the game.
    #[test]
    fn orb_cells_tile_the_sheet_by_row_and_column() {
        // Cell 0 is the top-left 16×16 square: u and v both 0..0.25.
        assert_eq!(
            experience_orb_cell_uvs(0),
            [[0.0, 0.25], [0.25, 0.25], [0.25, 0.0], [0.0, 0.0]]
        );
        // Cell 3 is the last column of row 0 — same v, u shifted three cells.
        assert_eq!(experience_orb_cell_uvs(3)[0], [0.75, 0.25]);
        // Cell 4 wraps to row 1: u back to 0, v advanced one cell.
        assert_eq!(experience_orb_cell_uvs(4)[0], [0.0, 0.5]);
        // Cell 10 is row 2, column 2 — the highest cell `experience_orb_icon` returns.
        assert_eq!(experience_orb_cell_uvs(10)[0], [0.5, 0.75]);
        // Every cell must fit inside the sheet, or the sampler clamps and two
        // different values draw the same edge texels.
        for icon in 0..EXPERIENCE_ORB_ICON_COUNT {
            for [u, v] in experience_orb_cell_uvs(icon) {
                assert!((0.0..=1.0).contains(&u), "cell {icon} u {u} off-sheet");
                assert!((0.0..=1.0).contains(&v), "cell {icon} v {v} off-sheet");
            }
        }
    }

    /// The orb quad sits **above** its own origin, and its vertical span is
    /// vanilla's after the scale and the lift.
    ///
    /// Predicted from the record's own constants rather than eyeballed: local
    /// `y ∈ [-0.25, 0.75]`, scaled by `0.3` and lifted `0.1`, is
    /// `[0.025, 0.325]`. The wrong hypothesis — a quad centred on its origin, which
    /// is what "billboard" suggests — would span `[-0.05, 0.25]` and bury the
    /// bottom sixth of every orb in the floor.
    #[test]
    fn orb_quad_sits_above_the_ground_after_the_scale_and_lift() {
        let feet = Vec3::new(4.0, 65.0, -9.0);
        // Identity orientation: a camera looking down -Z with no roll, which is
        // what `camera_orientation` returns for the default view. The vertical
        // extent is then the local one, scaled and lifted.
        let pose = experience_orb_matrix(feet, Mat4::IDENTITY);
        let bottom = pose.transform_point3(Vec3::new(0.0, -0.25, 0.0));
        let top = pose.transform_point3(Vec3::new(0.0, 0.75, 0.0));
        assert!(
            (bottom.y - (65.0 + 0.025)).abs() < 1.0e-5,
            "bottom at {}, expected {}",
            bottom.y,
            65.0 + 0.025
        );
        assert!(
            (top.y - (65.0 + 0.325)).abs() < 1.0e-5,
            "top at {}, expected {}",
            top.y,
            65.0 + 0.325
        );
        // The centred hypothesis, evaluated at this input and required to differ.
        assert!(
            (bottom.y - (65.0 - 0.05)).abs() > 1.0e-3,
            "a centred quad would also pass this"
        );
        // The orb is a fifth of a block wide at 0.3 scale: `x ∈ [-0.15, 0.15]`.
        let left = pose.transform_point3(Vec3::new(-0.5, 0.0, 0.0));
        assert!((left.x - (4.0 - 0.15)).abs() < 1.0e-5, "left at {}", left.x);
    }

    /// The `+7` block-light boost touches the block nibble only.
    ///
    /// A version that boosted the packed byte as a whole (`packed + 7`) agrees with
    /// this one on any byte whose block nibble is below 9 — so the discriminating
    /// input is a **saturating** one, where the correct answer clamps the nibble at
    /// 15 and the wrong one carries into the sky nibble.
    #[test]
    fn orb_light_boosts_only_the_block_nibble() {
        // Pitch black: block 0 -> 7, sky untouched.
        assert_eq!(experience_orb_light(0x00), 0x07);
        // Sky 15, block 0: the boost must not touch the sky nibble.
        assert_eq!(experience_orb_light(0xF0), 0xF7);
        // The discriminating case. Block 10 saturates at 15; a whole-byte `+7`
        // would give 0x51 — a *lower* sky level and a block level of 1.
        assert_eq!(experience_orb_light(0x4A), 0x4F);
        assert_ne!(experience_orb_light(0x4A), 0x4A_u8.wrapping_add(7));
    }

    /// Vanilla's orb colour pins green at full and modulates red far harder than
    /// blue. The wrong hypothesis is a symmetric hue cycle (equal amplitudes),
    /// which is what a transcription that dropped the `0.1` would produce.
    #[test]
    fn orb_tint_pins_green_and_modulates_red_ten_times_harder_than_blue() {
        // Sweep a whole `phase` period — `age/2`, so 4π ticks.
        let mut greens = Vec::new();
        let mut reds = Vec::new();
        let mut blues = Vec::new();
        for tick in 0..64 {
            let [r, g, b] = experience_orb_tint(tick as f32 / 4.0);
            greens.push(g);
            reds.push(r);
            blues.push(b);
        }
        assert!(greens.iter().all(|g| *g == 255), "green must never modulate");
        let red_span = reds.iter().max().copied().unwrap_or(0) - reds.iter().min().copied().unwrap_or(0);
        let blue_span =
            blues.iter().max().copied().unwrap_or(0) - blues.iter().min().copied().unwrap_or(0);
        // `0.5` vs `0.1` amplitude over the same `sin` range: 255 vs 51.
        assert!(red_span > 240, "red span {red_span}, expected the full 0.5 swing");
        assert!(
            (40..=60).contains(&blue_span),
            "blue span {blue_span}, expected ~51 from the 0.1 amplitude"
        );
        // The symmetric-amplitude hypothesis, evaluated here and required to fail.
        assert!(
            blue_span * 4 < red_span,
            "equal amplitudes would pass every assertion above"
        );
    }

    /// The other half of the drowned defect: even with its own mesh, a drowned
    /// wearing `zombie.png` still reads as an ordinary zombie. The path is
    /// derived from the corpus entry, so this pins the derivation, not a table.
    #[test]
    fn variant_mobs_point_at_their_own_sheet() {
        assert_eq!(
            entity_texture_candidates("drowned"),
            ["assets/minecraft/textures/entity/zombie/drowned.png"]
        );
        assert_eq!(
            entity_texture_candidates("husk"),
            ["assets/minecraft/textures/entity/zombie/husk.png"]
        );
        assert_eq!(
            entity_texture_candidates("stray"),
            ["assets/minecraft/textures/entity/skeleton/stray.png"]
        );
    }

    /// **The variant-resolver gate.** A wire variant must select the *breed's* own
    /// sheet, not the model's default.
    ///
    /// # The discriminating requirement
    ///
    /// "Returns `Some`" is satisfied by a resolver that hands back
    /// `default_path()`, which is exactly the behaviour this replaces — so every
    /// assertion below is stated as a **difference from the default**, and the nine
    /// breeds are required to be nine *distinct* sheets so no selector can be a
    /// constant function.
    ///
    /// # Where the expected values come from
    ///
    /// Vanilla's wolf-variant registration function (26.2 decompile), which is both halves of
    /// the answer and neither is guessable from the other: it builds the wild sheet
    /// as `"entity/wolf/" + file_name`, and it registers that file name against a
    /// **registry key** — `register(context, ASHEN, "wolf_ashen", …)`, with
    /// `ASHEN = createKey("ashen")`. So the wire's `minecraft:ashen` holder maps to
    /// the stem `wolf_ashen`, and `PALE` maps to the bare `wolf` rather than to
    /// `wolf_pale`, which is the one entry a uniform `"wolf_" + key` rule would get
    /// wrong. That asymmetry is asserted explicitly below.
    #[test]
    fn a_wire_variant_selects_the_breeds_own_sheet_not_the_models_default() {
        use lodestone_model::EntityVariant as Wire;

        let keyed = |path: &str| Wire::Keyed(format!("minecraft:{path}").parse().unwrap());
        let default = entity_models()
            .into_iter()
            .find(|e| e.name == "wolf")
            .expect("wolf is in the corpus")
            .texture
            .default_path();
        assert_eq!(default, "entity/wolf/wolf", "the default is the pale sheet");

        // (registry key, stem) straight off vanilla's wolf-variant bootstrap function.
        let vanilla: [(&str, &str); 9] = [
            ("pale", "wolf"),
            ("spotted", "wolf_spotted"),
            ("snowy", "wolf_snowy"),
            ("black", "wolf_black"),
            ("ashen", "wolf_ashen"),
            ("rusty", "wolf_rusty"),
            ("woods", "wolf_woods"),
            ("chestnut", "wolf_chestnut"),
            ("striped", "wolf_striped"),
        ];

        let mut wrong = Vec::new();
        let mut seen: Vec<&'static str> = Vec::new();
        for (key, stem) in vanilla {
            let want = format!("entity/wolf/{stem}");
            match entity_variant_sheet("wolf", &keyed(key)) {
                Some(got) => {
                    if got != want {
                        wrong.push(format!("{key}: want {want:?}, got {got:?}"));
                    }
                    seen.push(got);
                }
                None => wrong.push(format!("{key}: resolved to None")),
            }
        }
        // Eight of the nine must differ from the default; `pale` legitimately *is*
        // the default, which is why this is a count and not a blanket `!=`.
        let non_default = seen.iter().filter(|s| **s != default).count();
        if non_default != 8 {
            wrong.push(format!(
                "only {non_default} of 9 breeds resolved away from the default sheet \
                 — a resolver returning `default_path()` would score 0"
            ));
        }
        seen.sort_unstable();
        seen.dedup();
        if seen.len() != 9 {
            wrong.push(format!(
                "the nine breeds collapsed to {} distinct sheets, so the selector is \
                 not reading the coat",
                seen.len()
            ));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The climate axis, on the same wire shape. `pig`/`cow`/`chicken` all carry it,
    /// and all three currently draw `_temperate` for every animal in the world.
    #[test]
    fn a_climate_variant_selects_the_cold_and_warm_sheets() {
        use lodestone_model::EntityVariant as Wire;

        let keyed = |path: &str| Wire::Keyed(format!("minecraft:{path}").parse().unwrap());
        let mut wrong = Vec::new();
        for model in ["pig", "cow", "chicken"] {
            for climate in ["temperate", "cold", "warm"] {
                let want = format!("entity/{model}/{model}_{climate}");
                match entity_variant_sheet(model, &keyed(climate)) {
                    Some(got) if got == want => {}
                    other => wrong.push(format!("{model}/{climate}: want {want:?}, got {other:?}")),
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// **The controls.** Everything the resolver must decline, so "it returns a
    /// sheet" is demonstrably not unconditional.
    ///
    /// A foreign namespace is the one worth stating: a data pack's `mypack:ashen` is
    /// a different holder with no vanilla sheet, and a path-only comparison would
    /// hand it `wolf_ashen`.
    #[test]
    fn control_the_resolver_declines_what_it_cannot_map() {
        use lodestone_model::EntityVariant as Wire;

        let keyed = |id: &str| Wire::Keyed(id.parse().unwrap());
        let mut wrong = Vec::new();
        for (what, model, variant) in [
            (
                "a model with no variant axis",
                "zombie",
                keyed("minecraft:ashen"),
            ),
            ("an unknown breed key", "wolf", keyed("minecraft:nonesuch")),
            ("a foreign namespace", "wolf", keyed("mypack:ashen")),
            (
                "a wrong-axis variant (sheep dye on a wolf)",
                "wolf",
                Wire::Dyed {
                    color: 4,
                    sheared: false,
                },
            ),
            (
                "a model not in the corpus at all",
                "nonesuch",
                keyed("minecraft:ashen"),
            ),
        ] {
            if let Some(got) = entity_variant_sheet(model, &variant) {
                wrong.push(format!("{what}: expected None, got {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// **The tame gap, pinned as a fact rather than left as a comment.**
    ///
    /// [`entity_variant_sheet`] (the plain, 2-argument entry point every
    /// production caller still uses) has no way to *receive* a tame bit, so
    /// it must always pin `WolfState::Wild` — not because the wire carries
    /// nothing (it does, see [`entity_variant_sheet`]'s own doc), but because
    /// nothing upstream of its callers folds that wire field into anything
    /// this function's signature can see. This asserts that deliberate
    /// pinning; if it starts failing, `entity_variant_sheet` gained a way to
    /// see the bit without gaining a parameter for it, which would be a
    /// second, undocumented path — fix the signature instead of this
    /// assertion.
    ///
    /// The corpus already knows the tame sheet, and the assertion names it, so the
    /// gate cannot pass by the tame path merely being unimplemented in the corpus.
    /// 26.2 also ships a `_baby` axis (vanilla's wolf-variant registration function builds six
    /// identifiers, not three) which `WolfState` does not model at all.
    #[test]
    fn a_tamed_wolf_still_resolves_to_its_wild_sheet_through_the_plain_entry_point() {
        use lodestone_assets::entity::{EntityVariant as Axis, WolfCoat, WolfState};
        use lodestone_model::EntityVariant as Wire;

        let entry = entity_models()
            .into_iter()
            .find(|e| e.name == "wolf")
            .expect("wolf is in the corpus");
        let wild = entry.texture.resolve(Axis::Wolf {
            coat: WolfCoat::Ashen,
            state: WolfState::Wild,
        });
        let tame = entry.texture.resolve(Axis::Wolf {
            coat: WolfCoat::Ashen,
            state: WolfState::Tame,
        });
        assert_eq!(
            (wild, tame),
            ("entity/wolf/wolf_ashen", "entity/wolf/wolf_ashen_tame"),
            "the corpus must model both states, or the claim below is vacuous"
        );

        let got = entity_variant_sheet(
            "wolf",
            &Wire::Keyed("minecraft:ashen".parse().unwrap()),
        );
        assert_eq!(
            got,
            Some(wild),
            "the plain entry point has no tame parameter, so it must resolve wild"
        );
        assert_ne!(
            got,
            Some(tame),
            "if this now fails, `entity_variant_sheet` gained a tame source of its \
             own rather than `entity_variant_sheet_for` gaining a caller — thread \
             the ECS component through instead of changing this function's pin"
        );
    }

    /// The other half of the tame gap: [`entity_variant_sheet_for`] — the
    /// function that *can* see a tame bit — actually uses it. This is the
    /// positive proof the render-side fix works; what remains (an ECS
    /// component and a shell call site, both outside this crate) is named in
    /// [`entity_variant_sheet`]'s own doc comment.
    #[test]
    fn entity_variant_sheet_for_resolves_the_tame_sheet_when_told_the_wolf_is_tamed() {
        use lodestone_model::EntityVariant as Wire;

        let key = Wire::Keyed("minecraft:ashen".parse().unwrap());
        let wild = entity_variant_sheet_for("wolf", &key, false);
        let tame = entity_variant_sheet_for("wolf", &key, true);
        assert_eq!(wild, Some("entity/wolf/wolf_ashen"));
        assert_eq!(tame, Some("entity/wolf/wolf_ashen_tame"));
        assert_ne!(wild, tame, "the tame parameter must actually change the sheet");
        // A non-wolf model ignores the parameter entirely, matching
        // `entity_variant_sheet`'s existing per-model table (only `"wolf"`
        // has a `WolfState` axis at all).
        let pig_key = Wire::Keyed("minecraft:cold".parse().unwrap());
        assert_eq!(
            entity_variant_sheet_for("pig", &pig_key, false),
            entity_variant_sheet_for("pig", &pig_key, true),
        );
    }

    #[test]
    fn mesh_has_four_verts_and_six_indices_per_quad() {
        let mesh = pig_mesh();
        assert!(mesh.quad_count() > 0, "pig must produce geometry");
        assert_eq!(mesh.vertices.len(), mesh.quad_count() * 4);
        assert_eq!(mesh.indices.len(), mesh.quad_count() * 6);
        // Matches the underlying bake exactly (one quad per baked quad).
        let baked =
            lodestone_assets::entity::bake_entity(&lodestone_assets::entity_models::pig_model());
        assert_eq!(mesh.quad_count(), baked.len());
    }

    #[test]
    fn mesh_indices_are_all_in_range() {
        let mesh = pig_mesh();
        let n = mesh.vertices.len() as u32;
        assert!(mesh.indices.iter().all(|&i| i < n));
    }

    #[test]
    fn model_matrix_stands_the_mob_upright_at_its_feet() {
        // A humanoid: head cube top is model y = -8/16 = -0.5, feet reach
        // y = 24/16 = 1.5 (Y-down). After placement, feet ≈ world feet, head above.
        let feet = Vec3::new(10.0, 64.0, -20.0);
        let m = entity_model_matrix(feet, 0.0, 1.0);

        let model_feet = m.transform_point3(Vec3::new(0.0, 1.5, 0.0));
        let model_head = m.transform_point3(Vec3::new(0.0, -0.5, 0.0));

        // Feet land on the ground plane (within a couple of cm of the offset).
        assert!(
            (model_feet.y - feet.y).abs() < 0.05,
            "feet should sit at the entity position, got {model_feet:?}",
        );
        // Head is clearly above the feet: upright, not upside-down.
        assert!(
            model_head.y > model_feet.y + 1.5,
            "head must be above feet (upright), head={model_head:?} feet={model_feet:?}",
        );
        // Horizontal position stays at the feet column.
        assert!((model_feet.x - feet.x).abs() < 1e-4);
        assert!((model_feet.z - feet.z).abs() < 1e-4);
    }

    #[test]
    fn model_matrix_preserves_handedness() {
        // scale(-1,-1,1) has det +1, so combined with rotation/translation the
        // transform must preserve winding (positive determinant).
        let m = entity_model_matrix(Vec3::new(1.0, 2.0, 3.0), 37.0, 1.0);
        assert!(
            m.determinant() > 0.0,
            "det must stay positive so back-face culling remains valid, got {}",
            m.determinant(),
        );
    }

    #[test]
    fn upside_down_instance_rotates_body_and_attached_part_together() {
        let models = EntityModelSet::load();
        let anim = AnimInput {
            head_yaw_deg: 18.0,
            head_pitch_deg: -11.0,
            ..AnimInput::REST
        };
        let feet = Vec3::new(3.0, 64.0, -2.0);
        let base = models
            .resolve("zombie", feet, 37.0, 1.0, &anim)
            .expect("zombie model");
        let flipped = base
            .clone()
            .with_upside_down(feet, 37.0, 1.0, 1.95);
        assert_ne!(flipped.transform, base.transform);
        assert_eq!(flipped.part_transforms.len(), base.part_transforms.len());
        let head = models.get("zombie").unwrap().skeleton.index_of("head").unwrap();
        assert_ne!(flipped.part_transforms[head], base.part_transforms[head]);
        // The source placement order is scale, body yaw, translate by
        // `(bounding_box_height + 0.1) / scale`, rotate Z by 180 degrees,
        // then the model flip and -1.501 lift. For the generated zombie mesh
        // this predicts `64.01775..66.049` in world Y. The lower edge is
        // slightly above the ordinary lower edge because the baked model's
        // local Y extent is not exactly the reported 1.95-block box height;
        // asserting a direction alone would encode the wrong invariant.
        assert!((flipped.aabb_min.y - 64.01775).abs() < 1e-4);
        assert!((flipped.aabb_max.y - 66.049).abs() < 1e-4);
        assert!(flipped.aabb_min.y > base.aabb_min.y);
        assert!(flipped.aabb_max.y > base.aabb_max.y);

        // Control: applying the same helper with an ordinary model transform
        // is the only difference; a near-miss name must not be able to reuse
        // the flipped matrix by accident at this render seam.
        let ordinary = base.clone();
        assert_eq!(ordinary.transform, base.transform);
    }

    #[test]
    fn yaw_rotates_about_the_vertical_axis() {
        // A point offset in +X (model) with feet at origin: under a 90° body-yaw
        // change it must swing in the horizontal plane while its height is
        // unchanged (rotation is about Y only).
        let feet = Vec3::ZERO;
        let probe = Vec3::new(1.0, 0.5, 0.0);
        let a = entity_model_matrix(feet, 0.0, 1.0).transform_point3(probe);
        let b = entity_model_matrix(feet, 90.0, 1.0).transform_point3(probe);
        assert!(
            (a.y - b.y).abs() < 1e-5,
            "yaw must not change height: {a:?} vs {b:?}",
        );
        let horizontal = ((a.x - b.x).powi(2) + (a.z - b.z).powi(2)).sqrt();
        assert!(
            horizontal > 0.5,
            "a 90° yaw change must move the point horizontally: {a:?} vs {b:?}",
        );
    }

    #[test]
    fn scale_shrinks_the_model_about_the_feet() {
        let feet = Vec3::new(0.0, 0.0, 0.0);
        let full = entity_model_matrix(feet, 0.0, 1.0).transform_point3(Vec3::new(0.0, -0.5, 0.0));
        let baby = entity_model_matrix(feet, 0.0, 0.5).transform_point3(Vec3::new(0.0, -0.5, 0.0));
        // Feet stay near the ground for both; the head of the scaled mob is lower.
        assert!(baby.y < full.y, "scaled-down mob's head must be lower");
        assert!(baby.y > feet.y, "scaled mob still stands above its feet");
    }

    /// A zombie's resting arms stick out ~0.75 blocks in front of it, so its
    /// culling box has to be drawn around the mob *as posed*, not around a mob
    /// standing to attention. `EntityMesh::from_named_model` gets that by
    /// choosing the arm rig before taking the local bounds; if it did not, the
    /// error would be invisible until a zombie clipped out at the screen edge.
    #[test]
    fn a_zombies_local_bounds_include_its_outstretched_arms() {
        let plain = EntityMesh::from_model(&lodestone_assets::entity_models::zombie_model());
        let zombie = EntityMesh::from_named_model(
            "zombie",
            &lodestone_assets::entity_models::zombie_model(),
        );
        assert_eq!(
            humanoid_arms_for("zombie"),
            crate::entity_anim::HumanoidArms::Zombie
        );
        // Every model that calls vanilla's zombie-arms animation, so the set is not "zombie
        // plus whatever was remembered". `zombified_piglin` was the one missing.
        for name in ["husk", "drowned", "zombie_villager", "zombified_piglin"] {
            assert_eq!(
                humanoid_arms_for(name),
                crate::entity_anim::HumanoidArms::Zombie,
                "{name}'s model calls vanilla's zombie-arms animation"
            );
        }
        // Vanilla's giant renderer uses a bare base humanoid model, so a giant's arms hang.
        assert_eq!(
            humanoid_arms_for("giant"),
            crate::entity_anim::HumanoidArms::Swinging
        );
        // Model -Z is the mob's facing, so the arms extend the *minimum* Z.
        // The arm cube ends 10 texels (0.625 blocks) down from its pivot, so at
        // -80° it reaches ~0.63 blocks forward against an arms-down torso whose
        // frontmost point is the 0.28-block hat overlay.
        assert!(
            zombie.local_min.z < plain.local_min.z - 0.3,
            "the zombie's bounds reach {} forward against an arms-down {} — the rig was not \
             applied before the AABB was taken",
            zombie.local_min.z,
            plain.local_min.z
        );

        // And the bound must actually hold for every posed vertex.
        let feet = Vec3::new(5.0, 70.0, 5.0);
        let inst = EntityInstance::new("zombie", &zombie, feet, 37.0, 1.0, &AnimInput::REST);
        for (part, range) in zombie.parts.iter().enumerate() {
            let m = inst.part_transforms[part];
            let lo = range.vertex_start as usize;
            let hi = lo + range.vertex_count as usize;
            for v in &zombie.vertices[lo..hi] {
                let w = m.transform_point3(Vec3::from(v.position));
                assert!(
                    w.cmpge(inst.aabb_min - Vec3::splat(1e-2)).all()
                        && w.cmple(inst.aabb_max + Vec3::splat(1e-2)).all(),
                    "vertex {w:?} escaped AABB [{:?}, {:?}]",
                    inst.aabb_min,
                    inst.aabb_max,
                );
            }
        }
    }

    #[test]
    fn instance_world_aabb_contains_the_transformed_mesh() {
        let mesh = pig_mesh();
        let feet = Vec3::new(5.0, 70.0, 5.0);
        let inst = EntityInstance::new("pig", &mesh, feet, 45.0, 1.0, &AnimInput::REST);
        // Vertices are part-local, so a vertex only lands in the world once it
        // has been through *its own part's* matrix — the same matrix the GPU
        // draws it with. Using `inst.transform` alone would collapse every part
        // onto the model origin and the AABB check would be meaningless.
        for (part, range) in mesh.parts.iter().enumerate() {
            let m = inst.part_transforms[part];
            let lo = range.vertex_start as usize;
            let hi = lo + range.vertex_count as usize;
            for v in &mesh.vertices[lo..hi] {
                let w = m.transform_point3(Vec3::from(v.position));
                assert!(
                    w.cmpge(inst.aabb_min - Vec3::splat(1e-3)).all()
                        && w.cmple(inst.aabb_max + Vec3::splat(1e-3)).all(),
                    "vertex {w:?} escaped AABB [{:?}, {:?}]",
                    inst.aabb_min,
                    inst.aabb_max,
                );
            }
        }
    }

    #[test]
    fn armor_stand_base_plate_counter_rotates_with_the_entity() {
        let models = EntityModelSet::load();
        let mesh = models.get("armor_stand").expect("armor stand mesh");
        let base_plate = mesh
            .skeleton
            .index_of("base_plate")
            .expect("armor stand base plate");
        let pose = lodestone_model::ArmorStandPose::VANILLA_DEFAULT;
        let at = |yaw| {
            EntityInstance::new(
                "armor_stand",
                mesh,
                Vec3::new(5.0, 70.0, 5.0),
                yaw,
                1.0,
                &AnimInput {
                    armor_stand_pose: Some(pose),
                    ..AnimInput::REST
                },
            )
            .part_transforms[base_plate]
        };
        let facing_north = at(0.0);
        let facing_east = at(90.0);
        let max_delta = facing_north
            .to_cols_array()
            .into_iter()
            .zip(facing_east.to_cols_array())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(
            max_delta < 1.0e-5,
            "the world-facing base plate changed under a 90 degree body turn; max matrix delta={max_delta}"
        );
    }

    #[test]
    fn armor_stand_pose_culling_box_contains_every_posed_vertex() {
        let models = EntityModelSet::load();
        let mesh = models.get("armor_stand").expect("armor stand mesh");
        let pose = lodestone_model::ArmorStandPose {
            right_arm: lodestone_model::Vec3f::new(137.0, -23.0, 41.0),
            left_leg: lodestone_model::Vec3f::new(-89.0, 17.0, -31.0),
            ..lodestone_model::ArmorStandPose::VANILLA_DEFAULT
        };
        let inst = EntityInstance::new(
            "armor_stand",
            mesh,
            Vec3::new(5.0, 70.0, 5.0),
            37.0,
            1.0,
            &AnimInput {
                armor_stand_pose: Some(pose),
                ..AnimInput::REST
            },
        );
        let rest_box = transformed_aabb(&inst.transform, mesh.local_min, mesh.local_max);
        let differs_from_rest = (inst.aabb_min - rest_box.0).length() > 1.0e-3
            || (inst.aabb_max - rest_box.1).length() > 1.0e-3;
        assert!(
            differs_from_rest,
            "a discriminating non-rest pose must change the culling box; posed={:?}..{:?}, rest={:?}..{:?}",
            inst.aabb_min,
            inst.aabb_max,
            rest_box.0,
            rest_box.1
        );
        for (part, range) in mesh.parts.iter().enumerate() {
            let transform = inst.part_transforms[part];
            let start = range.vertex_start as usize;
            let end = start + range.vertex_count as usize;
            for vertex in &mesh.vertices[start..end] {
                let world = transform.transform_point3(Vec3::from(vertex.position));
                assert!(
                    world.cmpge(inst.aabb_min - Vec3::splat(1.0e-4)).all()
                        && world.cmple(inst.aabb_max + Vec3::splat(1.0e-4)).all(),
                    "posed vertex {world:?} escaped armor-stand AABB {:?}..{:?}",
                    inst.aabb_min,
                    inst.aabb_max
                );
            }
        }
    }

    fn frustum_looking_down_pos_z() -> Frustum {
        use crate::camera::Camera;
        Camera {
            position: Vec3::new(0.0, 64.0, 0.0),
            yaw: 0.0, // faces +Z
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: 1.0,
            near: 0.05,
            far: 128.0,
        }
        .frustum()
    }

    #[test]
    fn frustum_culls_entities_behind_the_camera() {
        let mesh = pig_mesh();
        let frustum = frustum_looking_down_pos_z();
        let in_front = EntityInstance::new(
            "pig",
            &mesh,
            Vec3::new(0.0, 63.0, 20.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );
        let behind = EntityInstance::new(
            "pig",
            &mesh,
            Vec3::new(0.0, 63.0, -20.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );

        let frame = plan_entities(&[in_front, behind], &frustum);
        assert_eq!(frame.stats.drawn, 1, "only the front entity draws");
        assert_eq!(frame.stats.culled_frustum, 1, "the one behind is culled");
        assert!(frame.stats.is_meaningful());
        assert_eq!(frame.instance_count(), 1);
    }

    #[test]
    fn plan_groups_instances_by_model_type() {
        let pig = pig_mesh();
        let cow = EntityMesh::from_model(&lodestone_assets::entity_models::cow_model());
        let frustum = frustum_looking_down_pos_z();
        let at = |model, m: &EntityMesh, z: f32| {
            EntityInstance::new(
                model,
                &m,
                Vec3::new(0.0, 63.0, z),
                0.0,
                1.0,
                &AnimInput::REST,
            )
        };
        let instances = [
            at("pig", &pig, 10.0),
            at("cow", &cow, 12.0),
            at("pig", &pig, 14.0),
            // one behind the camera to force a cull so the frame is meaningful.
            at("pig", &pig, -30.0),
        ];
        let frame = plan_entities(&instances, &frustum);
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert!(frame.stats.is_meaningful());

        let pig_batch = frame.batches.iter().find(|b| b.model == "pig").unwrap();
        let cow_batch = frame.batches.iter().find(|b| b.model == "cow").unwrap();
        assert_eq!(
            pig_batch.transforms.len(),
            2,
            "two visible pigs batch together"
        );
        assert_eq!(cow_batch.transforms.len(), 1);
    }

    #[test]
    fn model_set_resolves_types_and_skips_unknowns() {
        let set = EntityModelSet::load();
        assert!(!set.is_empty());
        assert_eq!(set.len(), entity_models().len());

        let feet = Vec3::new(0.0, 63.0, 10.0);
        let pig = set
            .resolve("pig", feet, 0.0, 1.0, &AnimInput::REST)
            .expect("pig resolves");
        assert_eq!(pig.model, "pig");
        assert_eq!(
            set.resolve("cave_spider", feet, 0.0, 1.0, &AnimInput::REST)
                .unwrap()
                .model,
            "cave_spider"
        );
        // Unknown type resolves to nothing (renderer skips it).
        assert!(
            set.resolve("experience_orb", feet, 0.0, 1.0, &AnimInput::REST)
                .is_none()
        );
        // The resolved instance's model is present in the set for upload.
        assert!(set.get(pig.model).is_some());
    }

    #[test]
    fn plan_seam_resolves_culls_and_skips_modelless_in_one_call() {
        let set = EntityModelSet::load();
        let frustum = frustum_looking_down_pos_z();
        // A mix mirroring a live scene: two drawable pigs, one drawable cow, a
        // modelless type that must be dropped (not culled), and one pig behind
        // the camera to force a real cull.
        // The two visible pigs carry *different* light so the batch's `lights`
        // can be checked to stay in step with its `transforms`: a batch that
        // merged or reordered them would still have the right length.
        let spawns = [
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, 10.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
            EntitySpawn {
                type_path: "cow",
                feet: Vec3::new(0.0, 63.0, 12.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: 0x0A, // block light 10, no sky: a torch-lit cow indoors
            },
            EntitySpawn {
                type_path: "experience_orb", // no model — dropped, not counted
                feet: Vec3::new(0.0, 63.0, 14.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, 16.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: 0x00, // pitch dark
            },
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, -30.0), // behind camera
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
        ];

        let frame = set.plan(spawns, &frustum);

        // The modelless dragon is dropped before culling, so `total` counts only
        // the four entities that had a model, and exactly one of those culled.
        assert_eq!(frame.stats.total, 4, "modelless types are not counted");
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert!(frame.stats.is_meaningful());
        let pig_batch = frame.batches.iter().find(|b| b.model == "pig").unwrap();
        assert_eq!(pig_batch.transforms.len(), 2, "two visible pigs batch");
        assert!(frame.batches.iter().any(|b| b.model == "cow"));

        // Light must ride through `plan` per instance and stay index-aligned
        // with `transforms` — the culled pig drops out of both, so the surviving
        // pair is the lit one then the dark one, in spawn order.
        assert_eq!(
            pig_batch.lights,
            vec![u32::from(ENTITY_FULLBRIGHT), 0x00],
            "per-entity light must survive resolve + cull in transform order"
        );
        let cow_batch = frame.batches.iter().find(|b| b.model == "cow").unwrap();
        assert_eq!(cow_batch.lights, vec![0x0A]);
        for batch in &frame.batches {
            assert_eq!(
                batch.lights.len(),
                batch.transforms.len(),
                "one light per instance, or the instance buffer would misalign"
            );
        }

        // The one-call seam is exactly manual resolve + plan_entities: same frame.
        let manual: Vec<EntityInstance> = spawns
            .iter()
            .filter_map(|s| {
                set.resolve(s.type_path, s.feet, s.body_yaw_deg, s.scale, &s.anim)
                    .map(|i| i.with_light(s.light))
            })
            .collect();
        let manual_frame = plan_entities(&manual, &frustum);
        assert_eq!(frame.batches, manual_frame.batches);
        assert_eq!(frame.instance_count(), manual_frame.instance_count());
    }

    // ---- dropped items ---------------------------------------------------

    /// A unit cube's six outward-wound faces, in `mesh_item_quads`' vertex
    /// order, as a stand-in for a baked block item's geometry.
    fn cube_face(dir: Direction) -> [Vec3; 4] {
        let n = match dir {
            Direction::East => Vec3::X,
            Direction::West => -Vec3::X,
            Direction::Up => Vec3::Y,
            Direction::Down => -Vec3::Y,
            Direction::South => Vec3::Z,
            Direction::North => -Vec3::Z,
        };
        let u = if n.x.abs() < 0.5 { Vec3::X } else { Vec3::Y };
        let v = n.cross(u);
        let centre = Vec3::splat(0.5) + n * 0.5;
        [
            centre - u * 0.5 - v * 0.5,
            centre + u * 0.5 - v * 0.5,
            centre + u * 0.5 + v * 0.5,
            centre - u * 0.5 + v * 0.5,
        ]
    }

    fn cube_quad(dir: Direction) -> BakedQuad {
        let p = cube_face(dir);
        BakedQuad {
            positions: [p[0].into(), p[1].into(), p[2].into(), p[3].into()],
            uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            direction: dir,
            cullface: None,
            tint_index: None,
            shade: true,
            layer: 0,
            anim: 0,
            sprite: 0,
        }
    }

    fn unit_cube_quads() -> Vec<BakedQuad> {
        [
            Direction::East,
            Direction::West,
            Direction::Up,
            Direction::Down,
            Direction::South,
            Direction::North,
        ]
        .into_iter()
        .map(cube_quad)
        .collect()
    }

    /// The signed screen area of a quad's first triangle after `m` — the sign
    /// `FrontFace::Ccw` + `cull_mode: Back` acts on.
    fn screen_area(m: Mat4, q: [Vec3; 4]) -> f32 {
        let p: Vec<Vec3> = q.iter().map(|v| m.project_point3(*v)).collect();
        let a = p[1] - p[0];
        let b = p[2] - p[0];
        a.x * b.y - a.y * b.x
    }

    #[test]
    fn the_bob_never_dips_below_the_entity_position() {
        // `sin(..) * 0.1 + 0.1` is bounded to 0.0..=0.2, so a dropped item
        // hovers and never sinks into the block it landed on.
        for tick in 0..400 {
            let age = tick as f32 * 0.5;
            let bob = item_bob_height(age, 1.234);
            assert!(
                (0.0..=2.0 * ITEM_BOB_AMPLITUDE + 1e-6).contains(&bob),
                "bob {bob} at age {age} escaped 0..=0.2"
            );
        }
    }

    #[test]
    fn the_bob_and_the_spin_have_vanillas_periods() {
        // Bob: sin(age/10 + offs) has period 20*PI ticks. Spin: age/20 + offs
        // radians, so a full turn is 40*PI ticks. Asserting the *ratio* as well
        // catches a swapped pair of divisors, which equal-period tests do not.
        let offs = 0.0;
        let bob_period = std::f32::consts::TAU * ITEM_BOB_TICKS_PER_RADIAN;
        assert!(
            (item_bob_height(0.0, offs) - item_bob_height(bob_period, offs)).abs() < 1e-4,
            "the bob must repeat after {bob_period} ticks"
        );
        let spin_period = std::f32::consts::TAU * ITEM_SPIN_TICKS_PER_RADIAN;
        assert!(
            (item_spin_radians(spin_period, offs) - item_spin_radians(0.0, offs)
                - std::f32::consts::TAU)
                .abs()
                < 1e-4,
            "the spin must complete exactly one turn after {spin_period} ticks"
        );
        assert!(
            (spin_period / bob_period - 2.0).abs() < 1e-4,
            "vanilla bobs twice per revolution"
        );
    }

    #[test]
    fn two_entities_do_not_bob_in_lockstep() {
        // The whole point of a per-entity phase: a pile of drops must not
        // pulse as one object.
        let offsets: Vec<f32> = (1..=8)
            .map(|id| item_bob_offset(EntityNetworkId::Server(id)))
            .collect();
        for (i, a) in offsets.iter().enumerate() {
            assert!(
                (0.0..std::f32::consts::TAU).contains(a),
                "phase {a} out of range"
            );
            for b in &offsets[i + 1..] {
                assert!((a - b).abs() > 1e-3, "ids share a phase: {a} vs {b}");
            }
        }
        // ...and it must be stable, or the item jitters instead of spinning.
        assert_eq!(
            item_bob_offset(EntityNetworkId::Server(7)),
            item_bob_offset(EntityNetworkId::Server(7))
        );
    }

    #[test]
    fn the_hover_lift_puts_the_lowest_point_one_pixel_up() {
        // Vanilla's `minOffsetY = -box.minY + 0.0625`, measured on the GROUND-
        // posed model. Under block/block's ground pose the unit cube is scaled
        // to 0.25 and centred on y = 3/16, so its base sits at 3/16 - 1/8.
        let quads = unit_cube_quads();
        let (min_y, max_y) = posed_item_y_extent(&quads, &BLOCK_ITEM_GROUND);
        assert!((min_y - (3.0 / 16.0 - 0.125)).abs() < 1e-5, "min_y = {min_y}");
        assert!((max_y - (3.0 / 16.0 + 0.125)).abs() < 1e-5, "max_y = {max_y}");

        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        let pose = dropped_item_matrix(Vec3::ZERO, 0.0, 0.0, &BLOCK_ITEM_GROUND, lift);
        let lowest = unit_cube_quads()
            .iter()
            .flat_map(|q| q.positions)
            .map(|p| pose.transform_point3(Vec3::from(p)).y)
            .fold(f32::INFINITY, f32::min);
        // At age 0 with phase 0 the bob is exactly its 0.1 midpoint.
        let expected = ITEM_MIN_HOVER_HEIGHT + item_bob_height(0.0, 0.0);
        assert!(
            (lowest - expected).abs() < 1e-5,
            "the posed model's base must float {expected} above the entity, got {lowest}"
        );
    }

    #[test]
    fn the_spin_is_about_the_entity_position_not_the_model_origin() {
        // The centring inside `display_matrix` is what makes the item rotate on
        // the spot. If it were dropped, the cube would orbit its own corner and
        // swing half a block sideways every revolution.
        let quads = unit_cube_quads();
        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        let feet = Vec3::new(10.0, 64.0, -3.0);
        for age in [0.0f32, 13.0, 27.5, 61.0] {
            let pose = dropped_item_matrix(feet, age, 0.4, &BLOCK_ITEM_GROUND, lift);
            let centre = pose.transform_point3(Vec3::splat(0.5));
            assert!(
                (centre.x - feet.x).abs() < 1e-4 && (centre.z - feet.z).abs() < 1e-4,
                "at age {age} the item centre drifted to {centre} from {feet}"
            );
        }
    }

    #[test]
    fn dropped_item_pose_preserves_winding() {
        // Derive the front-facing sign from the camera rather than asserting
        // "positive" or "negative" — the same discipline `item_render`'s
        // `winding_matches_the_world_camera` uses, and the reason that test
        // cannot be fooled by a misremembered glam/wgpu convention.
        //
        // The trap this pins: the GUI rule is that `gui_ortho * gui_item_pose`
        // matches `view_projection`'s determinant SIGN. Applying that to a
        // *world* pose — which is left-multiplied by that same
        // `view_projection` — inverts it. A world pose must have a POSITIVE
        // determinant, and the composition then inherits the camera's.
        //
        // The camera's own sign is deliberately not written down here. It is a
        // property of the projection (negative under a forward `[0,1]` one,
        // positive under reversed-Z, because mirroring the clip `z` axis flips a
        // 4x4 determinant) and it decides nothing the rasterizer can see, which
        // reads facing from projected `x`/`y` alone. All that is required is
        // that it be non-degenerate, so that the relative claims below have a
        // reference to be relative to.
        let camera = crate::camera::Camera {
            position: Vec3::new(0.5, 0.5, 4.0),
            yaw: 180.0,
            pitch: 0.0,
            ..crate::camera::Camera::default()
        };
        let world = camera.view_projection();
        assert!(
            world.determinant().abs() > 1.0e-6,
            "the reference camera's projection is degenerate ({}), so it cannot \
             supply a front-facing sign",
            world.determinant()
        );
        let front_sign = screen_area(world, cube_face(Direction::South)).signum();
        assert_eq!(
            screen_area(world, cube_face(Direction::North)).signum(),
            -front_sign,
            "the reference camera must disagree about the far face"
        );

        let quads = unit_cube_quads();
        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        // Several ages, so a spin angle cannot be what rescues the sign.
        for age in [0.0f32, 5.0, 17.0, 33.0, 70.0] {
            let pose = dropped_item_matrix(
                Vec3::new(0.5, 0.5, 0.0),
                age,
                0.0,
                &BLOCK_ITEM_GROUND,
                lift,
            );
            assert!(
                pose.determinant() > 0.0,
                "a world-space item pose must not flip handedness; det = {} at age {age}",
                pose.determinant()
            );
            let composed = world * pose;
            assert_eq!(
                composed.determinant().signum(),
                world.determinant().signum(),
                "view_projection * pose must keep the camera's winding at age {age}"
            );
            // And on-screen: whichever cube face currently points at the camera
            // must carry the front-facing sign.
            let towards_camera = if (item_spin_radians(age, 0.0) / std::f32::consts::TAU).fract()
                < 0.25
            {
                Direction::South
            } else {
                continue;
            };
            assert_eq!(
                screen_area(composed, cube_face(towards_camera)).signum(),
                front_sign,
                "the face turned towards the camera must survive back-face culling at age {age}"
            );
        }
    }

    #[test]
    fn vault_spin_degrees_predicts_the_exact_running_total() {
        // Magnitude, not sign: at game_time 37 with partial_tick 0.5 the answer
        // is exactly 375.0 degrees (37.5 ticks * 10 deg/tick), derived from the
        // constant outside the function under test rather than a plausible
        // round number.
        let deg = vault_spin_degrees(37, 0.5);
        assert!(
            (deg - 375.0).abs() < 1e-4,
            "expected 375.0 degrees at (37, 0.5), got {deg}"
        );
        // Two continuous samples one tick apart must differ by exactly
        // VAULT_SPIN_DEGREES_PER_TICK, matching vanilla's own display-item spin
        // update's constant per-tick step.
        let a = vault_spin_degrees(100, 0.25);
        let b = vault_spin_degrees(101, 0.25);
        assert!(
            (b - a - VAULT_SPIN_DEGREES_PER_TICK).abs() < 1e-4,
            "one tick later must advance by exactly {VAULT_SPIN_DEGREES_PER_TICK} degrees, got {}",
            b - a
        );
    }

    #[test]
    fn vault_display_item_centres_on_the_blocks_upper_middle() {
        // `T(0.5, 0.4, 0.5)`: the cluster's pivot sits above the block's floor
        // centre, not at its corner — getting this backwards buries the item in
        // the vault's base or floats it a whole block up.
        let block_pos = Vec3::new(4.0, 70.0, -9.0);
        for spin in [0.0f32, 90.0, 217.0] {
            // `display_matrix` recentres the baked item box on its own middle
            // (`T(-0.5,-0.5,-0.5)` innermost — see that function's doc), so
            // `Vec3::splat(0.5)`, not the model-space origin, is the point that
            // survives a rotation unmoved — the same probe
            // `the_spin_is_about_the_entity_position_not_the_model_origin`
            // uses above for exactly this reason.
            let pose = vault_display_item_matrix(
                block_pos,
                spin,
                Vec3::ZERO,
                &DisplayTransform::default(),
            );
            let pivot = pose.transform_point3(Vec3::splat(0.5));
            let expected = block_pos + Vec3::new(0.5, 0.4, 0.5);
            assert!(
                pivot.distance(expected) < 1e-4,
                "pivot at spin {spin} was {pivot}, expected {expected}"
            );
        }
    }

    #[test]
    fn vault_display_item_pose_preserves_winding() {
        // Same discipline as `dropped_item_pose_preserves_winding`: a
        // world-space pose must have a POSITIVE determinant regardless of the
        // spin angle, so it composes correctly with a negative-determinant
        // camera.
        for spin in [0.0f32, 45.0, 133.0, 270.0] {
            let pose = vault_display_item_matrix(
                Vec3::new(1.0, 65.0, 2.0),
                spin,
                Vec3::ZERO,
                &BLOCK_ITEM_GROUND,
            );
            assert!(
                pose.determinant() > 0.0,
                "a world-space vault item pose must not flip handedness; det = {} at spin {spin}",
                pose.determinant()
            );
        }
    }

    #[test]
    fn the_mesh_carries_the_world_light_not_the_gui_full_bright() {
        // The regression this guards: reusing `mesh_item_quads` verbatim nails
        // every vertex to GUI_ITEM_LIGHT, so a drop in a pitch-black cave glows
        // exactly as brightly as one at noon.
        let quads = unit_cube_quads();
        let dark = dropped_item_mesh(
            &quads,
            GuiLight::Side,
            &BLOCK_ITEM_GROUND,
            Vec3::ZERO,
            0.0,
            0.0,
            0x02,
        );
        assert!(!dark.vertices.is_empty(), "the cube must mesh to something");
        assert!(
            dark.vertices.iter().all(|v| v.light == 0x02),
            "every vertex must carry the sampled world light"
        );
        assert_eq!(dark.quad_count(), quads.len());
    }

    #[test]
    fn the_two_ground_transforms_are_selected_by_gui_light() {
        assert_eq!(ground_transform_for(GuiLight::Side), BLOCK_ITEM_GROUND);
        assert_eq!(ground_transform_for(GuiLight::Front), GENERATED_ITEM_GROUND);
        // The flat family is posed twice as large and one pixel lower; a swap
        // would halve every dropped block.
        const {
            assert!(GENERATED_ITEM_GROUND.scale[0] > BLOCK_ITEM_GROUND.scale[0]);
        }
    }

    /// The declared slot must **win**, and the fallback must still fire — the
    /// second half is the control. Without it a `ground_transform` that ignored
    /// its `display` argument entirely would pass on every vanilla item, because
    /// the constants happen to equal what vanilla declares.
    #[test]
    fn a_declared_ground_slot_beats_the_gui_light_fallback() {
        let odd = DisplayTransform {
            rotation: [0.0, 17.0, 0.0],
            translation: [0.0, 9.0, 0.0],
            scale: [0.125, 0.125, 0.125],
        };
        let declared = DisplayTransforms::NONE.with(DisplaySlot::Ground, odd);
        assert_eq!(
            ground_transform(&declared, GuiLight::Side),
            odd,
            "the model's own display.ground must be used, not the constant"
        );

        // Control: a chain that declares nothing falls back, and the two
        // fallbacks are still told apart by gui_light.
        assert_eq!(
            ground_transform(&DisplayTransforms::NONE, GuiLight::Side),
            BLOCK_ITEM_GROUND
        );
        assert_eq!(
            ground_transform(&DisplayTransforms::NONE, GuiLight::Front),
            GENERATED_ITEM_GROUND
        );

        // And an *explicitly declared* identity is honoured rather than being
        // mistaken for "absent" — the trap `DisplayTransforms::get` would fall
        // into here, since `get` cannot tell the two apart.
        let flat = DisplayTransforms::NONE.with(DisplaySlot::Ground, DisplayTransform::default());
        assert_eq!(
            ground_transform(&flat, GuiLight::Side),
            DisplayTransform::default()
        );
    }

    /// A left-hand slot with no left-hand data must mirror onto the right-hand
    /// one, as vanilla's display-transform deserializer does. `block/block` and
    /// `item/generated` both declare `thirdperson_righthand` and no
    /// `thirdperson_lefthand`, so without this every block in an off hand would
    /// be posed with the identity.
    // ---- held items, and the first-person arm ---------------------------

    fn player_mesh() -> EntityMesh {
        EntityMesh::from_named_model(
            "player_wide",
            &lodestone_assets::entity::player_model(false),
        )
    }

    /// A plausible `thirdperson_righthand`: vanilla's `item/handheld` declares
    /// `rotation [0, -90, 55]`, `translation [0, 4, 0.5]`, `scale [0.85, …]`.
    fn handheld_third_person() -> DisplayTransform {
        DisplayTransform {
            rotation: [0.0, -90.0, 55.0],
            translation: [0.0, 4.0, 0.5],
            scale: [0.85, 0.85, 0.85],
        }
    }

    #[test]
    fn the_held_item_x_offset_mirrors_between_hands_and_nothing_else_does() {
        // The only asymmetry the layer itself introduces is `±offsetX`. Isolate
        // it by handing both arms the *same* identity display transform and the
        // same arm matrix, so any other difference would have to come from this
        // function.
        let flat = DisplayTransform::default();
        let right = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &flat);
        let left = held_item_matrix(Mat4::IDENTITY, Arm::Left, false, &flat);
        let r = right.transform_point3(Vec3::splat(0.5));
        let l = left.transform_point3(Vec3::splat(0.5));
        // Rx(-90) then Ry(180) sends the +x offset to -x, so the sign is flipped
        // once more than the naive reading — which is exactly why this is
        // measured rather than asserted from the constant.
        assert!((r.y - l.y).abs() < 1e-6, "y must not mirror: {r} vs {l}");
        assert!((r.z - l.z).abs() < 1e-6, "z must not mirror: {r} vs {l}");
        assert!(
            (r.x + l.x).abs() < 1e-6 && r.x.abs() > 1e-3,
            "x must mirror about zero and be non-zero: {r} vs {l}"
        );
    }

    #[test]
    fn a_baby_holds_its_item_closer_in() {
        // The baby triple is smaller on every axis, so the item sits nearer the
        // shoulder. A swapped adult/baby branch is the failure this catches.
        let t = handheld_third_person();
        let adult = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &t)
            .transform_point3(Vec3::splat(0.5));
        let baby =
            held_item_matrix(Mat4::IDENTITY, Arm::Right, true, &t).transform_point3(Vec3::splat(0.5));
        assert!(
            baby.length() < adult.length(),
            "the baby offset must be nearer the pivot: {baby} vs {adult}"
        );
    }

    #[test]
    fn the_held_item_pose_hangs_off_the_arm_matrix_it_is_given() {
        // The seam that makes this non-island: the caller passes
        // `part_transforms[arm]`, and translating that must translate the item
        // by exactly the same amount.
        let t = handheld_third_person();
        let base = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &t);
        let shift = Vec3::new(3.0, 64.0, -7.0);
        let moved = held_item_matrix(
            Mat4::from_translation(shift),
            Arm::Right,
            false,
            &t,
        );
        let a = base.transform_point3(Vec3::splat(0.5));
        let b = moved.transform_point3(Vec3::splat(0.5));
        assert!((b - a - shift).length() < 1e-4, "{a} -> {b}, expected +{shift}");
    }

    #[test]
    fn the_held_item_pose_preserves_winding_for_a_real_mob() {
        // Same discipline as `dropped_item_pose_preserves_winding`: the whole
        // chain is a *world* pose left-multiplied by `view_projection`, so its
        // determinant must be POSITIVE and the composition must inherit the
        // camera's negative sign. The GUI rule ("negative") applied here ships an
        // item you see the inside of, which still looks like a sword.
        let camera = crate::camera::Camera {
            position: Vec3::new(0.5, 1.0, 4.0),
            yaw: 180.0,
            pitch: 0.0,
            ..crate::camera::Camera::default()
        };
        let world = camera.view_projection();
        let front_sign = screen_area(world, cube_face(Direction::South)).signum();
        assert_eq!(
            screen_area(world, cube_face(Direction::North)).signum(),
            -front_sign,
            "the reference camera must disagree about the far face"
        );

        let mesh = player_mesh();
        let t = handheld_third_person();
        for yaw in [0.0f32, 37.0, 180.0, 271.0] {
            for (scale, baby) in [(1.0f32, false), (0.5, true)] {
                let inst = EntityInstance::new(
                    "player_wide",
                    &mesh,
                    Vec3::new(0.5, 0.0, 0.0),
                    yaw,
                    scale,
                    &AnimInput::REST,
                );
                for hand in [Arm::Right, Arm::Left] {
                    let arm_transform = inst
                        .hand_transform(hand)
                        .expect("player_wide has both arms");
                    let pose = held_item_matrix(arm_transform, hand, baby, &t);
                    assert!(
                        pose.determinant() > 0.0,
                        "a world-space held-item pose must not flip handedness; det = {} \
                         (yaw {yaw}, scale {scale}, {hand:?})",
                        pose.determinant()
                    );
                    assert_eq!(
                        (world * pose).determinant().signum(),
                        world.determinant().signum(),
                        "view_projection * pose must keep the camera's winding"
                    );
                }
            }
        }
    }

    /// The whole reason [`EntityInstance::hand_transform`] exists rather than
    /// reusing `part_transforms[skeleton.index_of(arm.part_name())]`: for a
    /// skeleton the two must actually differ (by the pivot shift), and the
    /// arm's *own* body-mesh transform (`part_transforms`) must stay exactly
    /// what it was — proof this crate's override never leaks into the
    /// whole-body draw it shares an index with.
    #[test]
    fn a_skeletons_hand_transform_differs_from_its_arms_body_transform() {
        let mesh = EntityMesh::from_named_model(
            "skeleton",
            &lodestone_assets::entity_models::skeleton_model(),
        );
        let inst = EntityInstance::new(
            "skeleton",
            &mesh,
            Vec3::new(0.5, 0.0, 0.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );
        let arm_idx = mesh.skeleton.index_of("right_arm").unwrap();
        let body_mesh_transform = inst.part_transforms[arm_idx];
        let hand_transform = inst.hand_transform(Arm::Right).unwrap();
        assert!(
            (hand_transform.w_axis - body_mesh_transform.w_axis).length() > 1e-4,
            "the pivot shift did not reach the hand transform: {hand_transform:?} vs \
             {body_mesh_transform:?}"
        );
    }

    #[test]
    fn hand_transform_picks_the_slot_the_arm_and_person_name() {
        let third = handheld_third_person();
        let first = DisplayTransform {
            rotation: [0.0, -90.0, 25.0],
            translation: [1.13, 3.2, 1.13],
            scale: [0.68, 0.68, 0.68],
        };
        let d = DisplayTransforms::NONE
            .with(DisplaySlot::ThirdPersonRightHand, third)
            .with(DisplaySlot::FirstPersonRightHand, first);
        assert_eq!(hand_transform(&d, Arm::Right, false), third);
        assert_eq!(hand_transform(&d, Arm::Right, true), first);
        // Both left slots are undeclared, so both fall back to their right-hand
        // partner — vanilla's deserializer rule, and the reason a block in an off
        // hand is not identity-posed.
        assert_eq!(hand_transform(&d, Arm::Left, false), third);
        assert_eq!(hand_transform(&d, Arm::Left, true), first);
        // ...and a model that declares nothing gets NO_TRANSFORM, not a guess.
        assert_eq!(
            hand_transform(&DisplayTransforms::NONE, Arm::Right, false),
            DisplayTransform::default()
        );
    }

    #[test]
    fn the_player_arm_rest_pose_is_a_pure_translation() {
        // What makes `rest_pose()[arm] * Rz(±0.1)` *exact* rather than an
        // approximation of `arm.resetPose(); arm.zRot = ±0.1`: the authored rest
        // rotation is zero and the root above it is the identity, so replacing
        // zRot is the same as post-multiplying Rz. If a future corpus edit gave
        // the player arm a rest rotation, this fails instead of silently drifting.
        let mesh = player_mesh();
        for arm in [Arm::Right, Arm::Left] {
            let i = mesh.skeleton.index_of(arm.part_name()).expect("arm part");
            let rest = mesh.skeleton.rest_pose()[i];
            let expect = Mat4::from_translation(Vec3::new(
                arm.invert() * -5.0 / 16.0,
                2.0 / 16.0,
                0.0,
            ));
            assert!(
                (rest - expect).to_cols_array().iter().all(|v| v.abs() < 1e-6),
                "{arm:?} rest pose must be PartPose::offset(∓5, 2, 0) with no rotation; got {rest}"
            );
        }
    }

    #[test]
    fn the_sleeve_shares_the_arms_matrix_exactly() {
        // `right_sleeve` is `PartPose::ZERO` under `right_arm`, so one uploaded
        // matrix drives both parts. Drawing the sleeve with its own recomputed
        // matrix would be the same number; drawing it with the *body's* would
        // put a floating sleeve mid-screen.
        let mesh = player_mesh();
        let rest = mesh.skeleton.rest_pose();
        for arm in [Arm::Right, Arm::Left] {
            let a = mesh.skeleton.index_of(arm.part_name()).expect("arm");
            let s = mesh
                .skeleton
                .index_of(arm.sleeve_part_name())
                .expect("sleeve");
            assert!(
                (rest[a] - rest[s]).to_cols_array().iter().all(|v| v.abs() < 1e-6),
                "{arm:?} sleeve must share the arm's matrix"
            );
            let parts = first_person_arm_parts(&mesh, arm);
            assert_eq!(parts, vec![a, s], "both parts must be drawn, arm first");
        }
        // A rig with no sleeve yields just the arm, and a rig with no arm at all
        // yields nothing — the control that keeps the `Vec` honest.
        assert_eq!(
            first_person_arm_parts(&pig_mesh(), Arm::Right),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn the_first_person_arm_lands_in_the_bottom_right_of_frame() {
        // Hand-computed from vanilla's own player-arm render chain with attack = 0 and
        // inverse_arm_height = 0, in camera space (x right, y up, -z forward):
        // the arm cube spans roughly x 0.33..0.91, y -0.99..-0.29, z -1.19..-0.44.
        // The load-bearing claims are the *signs*: right of centre, below the
        // eye, and in front of it. A missing rotation in the chain flips one.
        let mesh = player_mesh();
        let pose =
            first_person_arm_pose(&mesh, Arm::Right, 0.0).expect("player_wide has a right arm");
        // `player_wide`'s right arm cube: from [-3, -2, -2], size [4, 12, 4].
        let corners: Vec<Vec3> = (0..8u32)
            .map(|i| {
                let x = if i & 1 == 0 { -3.0f32 } else { 1.0 };
                let y = if i & 2 == 0 { -2.0f32 } else { 10.0 };
                let z = if i & 4 == 0 { -2.0f32 } else { 2.0 };
                pose.transform_point3(Vec3::new(x, y, z) / 16.0)
            })
            .collect();
        let lo = corners.iter().copied().reduce(Vec3::min).unwrap();
        let hi = corners.iter().copied().reduce(Vec3::max).unwrap();
        assert!(lo.x > 0.2 && hi.x < 1.1, "x span {}..{}", lo.x, hi.x);
        assert!(hi.y < -0.2 && lo.y > -1.2, "y span {}..{}", lo.y, hi.y);
        assert!(hi.z < -0.3 && lo.z > -1.4, "z span {}..{}", lo.z, hi.z);
        // Beyond the near plane, or the arm is clipped away entirely.
        assert!(hi.z < -HAND_NEAR, "the arm must be past the near plane");

        // The left arm is the mirror image about x, to within the zRot sign.
        let left = first_person_arm_pose(&mesh, Arm::Left, 0.0).expect("left arm");
        let lc = left.transform_point3(Vec3::ZERO);
        let rc = pose.transform_point3(Vec3::ZERO);
        assert!((lc.x + rc.x).abs() < 1e-4, "left/right must mirror: {lc} vs {rc}");
        assert!((lc.y - rc.y).abs() < 1e-4 && (lc.z - rc.z).abs() < 1e-4);
    }

    #[test]
    fn first_person_arm_pose_preserves_winding() {
        // The arm is drawn with the HUD projection alone, and a view matrix has
        // determinant +1, so `sign(det(hand_projection))` equals
        // `sign(det(view_projection))`. The arm pose must therefore be
        // orientation-*preserving*, like a world model matrix — not
        // orientation-reversing like the GUI item pose.
        let camera = crate::camera::Camera::default();
        let world = camera.view_projection();
        assert!(
            camera.view_matrix().determinant() > 0.0,
            "a view matrix must have determinant +1; that is why the two signs agree"
        );
        let proj = hand_projection(16.0 / 9.0);
        assert_eq!(
            proj.determinant().signum(),
            world.determinant().signum(),
            "hand_projection must share view_projection's handedness \
             (proj {}, world {})",
            proj.determinant(),
            world.determinant()
        );

        let mesh = player_mesh();
        for arm in [Arm::Right, Arm::Left] {
            // Every phase of the swing, not just rest: a rotation cannot change a
            // determinant's sign, but the chain is edited by hand and a stray
            // reflection (a negated scale, a mirrored translation folded into a
            // rotation) would only show up mid-swing.
            for step in 0..=8 {
                let attack = step as f32 / 8.0;
                let pose = first_person_arm_pose(&mesh, arm, attack).expect("arm");
                assert!(
                    pose.determinant() > 0.0,
                    "{arm:?} arm pose must not flip handedness at attack {attack}; det = {}",
                    pose.determinant()
                );
                assert_eq!(
                    (proj * pose).determinant().signum(),
                    world.determinant().signum(),
                    "hand_projection * arm pose must keep the world's winding at attack {attack}"
                );
            }
        }
    }

    /// The swing must be **additive**: `attack_anim == 0` has to reproduce the
    /// pre-swing chain exactly, or every existing framing assertion above (and the
    /// shell's headless arm gate) is silently measuring a different arm.
    ///
    /// The expected matrix is written out longhand rather than taken from
    /// `first_person_arm_chain` itself — comparing the function to itself would
    /// pass for any pair of symmetric mistakes.
    #[test]
    fn arm_chain_at_rest_matches_the_static_chain() {
        for arm in [Arm::Right, Arm::Left] {
            let i = arm.invert();
            let expected = Mat4::from_translation(Vec3::new(i * 0.640_000_05, -0.6, -0.719_999_97))
                * Mat4::from_rotation_y((i * 45.0).to_radians())
                * Mat4::from_translation(Vec3::new(i * -1.0, 3.6, 3.5))
                * Mat4::from_rotation_z((i * 120.0).to_radians())
                * Mat4::from_rotation_x(200.0f32.to_radians())
                * Mat4::from_rotation_y((i * -135.0).to_radians())
                * Mat4::from_translation(Vec3::new(i * 5.6, 0.0, 0.0));
            let actual = first_person_arm_chain(arm, 0.0);
            let delta = (expected - actual)
                .to_cols_array()
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(delta < 1e-5, "{arm:?} rest chain drifted by {delta}");
        }
        // The control: something must actually change once the swing is running,
        // or "rest matches" is satisfied by a chain that ignores `attack_anim`.
        let moved = (first_person_arm_chain(Arm::Right, 0.0)
            - first_person_arm_chain(Arm::Right, 0.4))
        .to_cols_array()
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(moved > 0.05, "the swing must move the chain, moved by {moved}");
    }

    /// A drawn bow does not merely select `bow_pulling_2` geometry.  Vanilla's
    /// first-person item-in-hand renderer replaces the ordinary held-item chain with this
    /// BOW transform while the use button is down.
    ///
    /// # Why this gate is written against two *wrong* hypotheses as well
    ///
    /// The version this replaced restated the implementation's own chain as its
    /// expected matrix, so it agreed with the code by construction and stayed
    /// green for as long as the bow was invisible in live play. Both real
    /// divergences it could not see are asserted here as named alternatives that
    /// the measurement must land *away* from:
    ///
    /// * **no leading `applyItemArmTransform`** — the chain starting at the
    ///   BOW-specific translation, which is what shipped;
    /// * **the shake folded into the leading translation's `y`** — displacing
    ///   along camera-space Y instead of the rotated local Y.
    ///
    /// `inverse_arm_height` is deliberately `0.35` rather than `0.0`: at zero the
    /// dip term vanishes and the correct chain coincides with one that never
    /// threaded the equip height through at all, so zero cannot discriminate.
    /// `held_ticks` is `20.0` (full charge, `power == 1.0`) because the shake is
    /// largest there and a zero shake would collapse the second hypothesis into
    /// the first.
    #[test]
    fn charged_bow_pose_matches_vanillas_item_in_hand_transform() {
        let arm = Arm::Right;
        let i = arm.invert();
        let held_ticks = 20.0f32;
        let inverse_arm_height = 0.35f32;
        let power = 1.0f32;
        assert_eq!(
            first_person_bow_power(held_ticks),
            power,
            "full bow charge is 20 elapsed ticks"
        );
        let shake = lodestone_physics::mth::sin(f64::from((held_ticks - 0.1) * 1.3))
            * (power - 0.1)
            * 0.004;
        assert!(
            shake.abs() > 1e-4,
            "the fixture must charge far enough for a shake to exist, got {shake}"
        );

        // Vanilla's held-item hand-layer submit function applies the item-arm
        // transform before the switch
        // for every animation whose `hasCustomArmTransform()` is false, and
        // the bow use-animation state's is false.
        let arm_transform =
            Mat4::from_translation(Vec3::new(i * 0.56, -0.52 + inverse_arm_height * -0.6, -0.72));
        // Everything from `case BOW:` onward, in source order.
        let bow_case = Mat4::from_translation(Vec3::new(i * -0.278_568_2, 0.183_443_87, 0.157_315_31))
            * Mat4::from_rotation_x((-13.935f32).to_radians())
            * Mat4::from_rotation_y((i * 35.3).to_radians())
            * Mat4::from_rotation_z((i * -9.785).to_radians())
            * Mat4::from_translation(Vec3::new(0.0, shake, 0.0))
            * Mat4::from_translation(Vec3::new(0.0, 0.0, power * 0.04))
            * Mat4::from_scale(Vec3::new(1.0, 1.0, 1.0 + power * 0.2))
            * Mat4::from_rotation_y((i * -45.0).to_radians());
        // Vanilla's no-transform constant still centres the model cube.
        let centre = Mat4::from_translation(Vec3::splat(-0.5));

        let expected = arm_transform * bow_case * centre;
        let without_arm_transform = bow_case * centre;
        let shake_in_leading_translation = arm_transform
            * Mat4::from_translation(Vec3::new(
                i * -0.278_568_2,
                0.183_443_87 + shake,
                0.157_315_31,
            ))
            * Mat4::from_rotation_x((-13.935f32).to_radians())
            * Mat4::from_rotation_y((i * 35.3).to_radians())
            * Mat4::from_rotation_z((i * -9.785).to_radians())
            * Mat4::from_translation(Vec3::new(0.0, 0.0, power * 0.04))
            * Mat4::from_scale(Vec3::new(1.0, 1.0, 1.0 + power * 0.2))
            * Mat4::from_rotation_y((i * -45.0).to_radians())
            * centre;

        let actual = first_person_bow_matrix(
            arm,
            held_ticks,
            inverse_arm_height,
            &DisplayTransform::default(),
        );
        let spread = |a: Mat4, b: Mat4| {
            (a - b)
                .to_cols_array()
                .iter()
                .fold(0.0f32, |max, value| max.max(value.abs()))
        };

        let delta = spread(expected, actual);
        assert!(
            delta < 1e-5,
            "a charging bow must use vanilla's item-in-hand renderer BOW pose, drifted by {delta}"
        );
        // Both alternatives are stated as distances rather than as a bare
        // "differs", so a chain that drifted toward either one fails with the
        // number rather than silently satisfying an inequality.
        let missing_arm = spread(without_arm_transform, actual);
        assert!(
            missing_arm > 0.5,
            "omitting applyItemArmTransform must move the pose by the arm offset, \
             but the two chains sit {missing_arm} apart"
        );
        let misplaced_shake = spread(shake_in_leading_translation, actual);
        assert!(
            misplaced_shake > 1e-4,
            "the shake belongs after the rotations, but both placements agree to \
             {misplaced_shake}"
        );
    }

    /// The equip dip must actually reach the bow pose.
    ///
    /// The gate above pins one `inverse_arm_height`; this one pins the *response*,
    /// because a chain that accepted the parameter and dropped it would still
    /// satisfy a single-point comparison written against the same value. Vanilla's
    /// coefficient is `-0.6` per unit, applied to `y` before any rotation, so a
    /// full swap lowers the whole chain by exactly that.
    #[test]
    fn bow_pose_dips_with_the_equip_height() {
        let rested = first_person_bow_chain(Arm::Right, 20.0, 0.0);
        let swapping = first_person_bow_chain(Arm::Right, 20.0, 1.0);
        let drop = swapping.w_axis - rested.w_axis;
        assert!(
            (drop.y - FIRST_PERSON_ITEM_EQUIP_DIP).abs() < 1e-6,
            "a full swap must lower the bow by {FIRST_PERSON_ITEM_EQUIP_DIP}, got {}",
            drop.y
        );
        assert!(
            drop.x.abs() < 1e-6 && drop.z.abs() < 1e-6,
            "the dip is vertical only, got {drop:?}"
        );
    }

    /// The five swing scalars against hand-evaluated vanilla values.
    ///
    /// `a = 0.25` is chosen because `sqrt(0.25) = 0.5` **exactly**, so every
    /// expected number below is a closed form off the unit circle rather than
    /// something read back out of this code:
    ///
    /// ```text
    /// xs = -0.3 · sin(0.5π)    = -0.3 · 1          = -0.3
    /// ys =  0.4 · sin(1.0π)    =  0.4 · 0          =  0.0
    /// zs = -0.4 · sin(0.25π)   = -0.4 · √2/2       = -0.28284271
    /// yr =        sin(0.5π)    =  1                =  1.0
    /// zr =        sin(0.0625π) =  sin(11.25°)      =  0.19509032
    /// ```
    ///
    /// This is where the `sqrt` shaping is actually pinned. A linear ramp gives
    /// `xs = -0.3·sin(0.25π) = -0.212`, `yr = 0.707` instead of `1.0` — the arm
    /// still swings, just wrongly, which is exactly the failure the matrix-level
    /// and pixel-level gates cannot distinguish.
    ///
    /// `ys == 0` here is not a weak assertion, it is the `2π` term crossing zero
    /// a quarter of the way in; a `π` typo would give `0.4` and fail loudly.
    #[test]
    fn arm_swing_terms_match_hand_evaluated_vanilla() {
        let t = ArmSwingTerms::new(0.25);
        assert!((t.x_position - -0.3).abs() < 1e-6, "xs {}", t.x_position);
        assert!(t.y_position.abs() < 1e-6, "ys {}", t.y_position);
        assert!(
            (t.z_position - -0.282_842_71).abs() < 1e-6,
            "zs {}",
            t.z_position
        );
        assert!((t.y_rotation - 1.0).abs() < 1e-6, "yr {}", t.y_rotation);
        assert!(
            (t.z_rotation - 0.195_090_32).abs() < 1e-6,
            "zr {}",
            t.z_rotation
        );

        // At a = 1.0 the arm is back at rest in x and y (both `sin` arguments are
        // whole multiples of π) — the property that makes the wrapped
        // `attack_anim_lerp` in `lodestone_entity::pose` land the arm at rest
        // rather than mid-arc.
        let end = ArmSwingTerms::new(1.0);
        assert!(end.x_position.abs() < 1e-6, "xs at end {}", end.x_position);
        assert!(end.y_position.abs() < 1e-6, "ys at end {}", end.y_position);
        assert!(end.y_rotation.abs() < 1e-6, "yr at end {}", end.y_rotation);

        // Every term is zero at rest, which is what `arm_chain_at_rest_matches_
        // the_static_chain` depends on.
        let rest = ArmSwingTerms::new(0.0);
        for (name, v) in [
            ("xs", rest.x_position),
            ("ys", rest.y_position),
            ("zs", rest.z_position),
            ("yr", rest.y_rotation),
            ("zr", rest.z_rotation),
        ] {
            assert_eq!(v, 0.0, "{name} must be 0 at rest");
        }

        // Out of range clamps rather than extrapolating.
        assert_eq!(ArmSwingTerms::new(-1.0).y_rotation, rest.y_rotation);
        assert_eq!(ArmSwingTerms::new(4.0).y_rotation, end.y_rotation);
    }

    // ---- the deferred third-person body: `EntityInstance::part_transforms`,
    // not `first_person_arm_pose` -- see that function's doc comment for why
    // sharing a code path would silently give one of the two the other's pose.

    fn player_slim_mesh() -> EntityMesh {
        EntityMesh::from_named_model("player_slim", &lodestone_assets::entity::player_model(true))
    }

    #[test]
    fn player_model_name_selects_wide_or_slim() {
        assert_eq!(player_model_name(false), "player_wide");
        assert_eq!(player_model_name(true), "player_slim");
        // Both names must be real corpus entries in their own right (not just
        // `canonical_model_name`'s hidden alias target), since a caller with
        // real skin data passes this straight through as a `type_path` — and
        // neither name is a `minecraft:entity_type` registry entry at all
        // (there is no `EntityType::PlayerWide`), so this goes through the
        // surviving `&str` boundary, `canonical_model_name`, not
        // `model_for_type`.
        assert_eq!(canonical_model_name("player_wide"), Some("player_wide"));
        assert_eq!(canonical_model_name("player_slim"), Some("player_slim"));
    }

    /// Vanilla draws two layers per limb: the base skin cube, and a slightly
    /// grown (scaled-up) overlay (`hat`/`jacket`/`right_sleeve`/`left_sleeve`/
    /// `right_pants`/`left_pants`) parented to it at `PartPose::ZERO`.
    /// Omitting the overlay looks like a missing-skin-layer bug, not a missing
    /// feature, so this pins that every overlay part is (a) present in the
    /// baked mesh and (b) posed *exactly* onto its base part by the animated
    /// third-person chain -- not just at rest, where a `ZERO`-pose child would
    /// trivially agree with its parent even if the composition were wrong.
    #[test]
    fn outer_layer_parts_follow_their_base_part_exactly() {
        for (name, mesh) in [("player_wide", player_mesh()), ("player_slim", player_slim_mesh())] {
            let anim = AnimInput {
                head_yaw_deg: 25.0,
                head_pitch_deg: -15.0,
                limb_swing: 3.7,
                limb_swing_amount: 1.0,
                attack_anim: 0.0,
                age_ticks: 40.0,
                aggressive: false,
                ..AnimInput::REST
            };
            let instance =
                EntityInstance::new(name, &mesh, Vec3::new(1.0, 0.0, 2.0), 37.0, 1.0, &anim);
            let pairs = [
                ("head", "hat"),
                ("body", "jacket"),
                ("right_arm", "right_sleeve"),
                ("left_arm", "left_sleeve"),
                ("right_leg", "right_pants"),
                ("left_leg", "left_pants"),
            ];
            for (base, overlay) in pairs {
                let bi = mesh.skeleton.index_of(base).unwrap_or_else(|| panic!("{name}.{base}"));
                let oi =
                    mesh.skeleton.index_of(overlay).unwrap_or_else(|| panic!("{name}.{overlay}"));
                let b = instance.part_transforms[bi].to_cols_array();
                let o = instance.part_transforms[oi].to_cols_array();
                for i in 0..16 {
                    assert!(
                        (b[i] - o[i]).abs() < 1e-5,
                        "{name}: {overlay} must be posed exactly onto {base} (a PartPose::ZERO \
                         child), element {i} differs: {} vs {}",
                        b[i],
                        o[i]
                    );
                }
            }
        }
    }

    /// The whole-body third-person chain is
    /// `entity_model_matrix(feet, yaw, scale) * Skeleton::pose(anim)[part]`
    /// (see [`EntityInstance::new`]) -- the *same* `scale(-1,-1,1)`-carrying
    /// placement matrix the module doc already proves has determinant `+1` for
    /// any rigid part chain, just exercised over every part of a real player
    /// mesh (including the outer-layer overlays) instead of asserted once in
    /// prose. A negative determinant here would mean a player rendered
    /// inside-out the moment a third-person camera exists to look at one.
    #[test]
    fn third_person_body_part_transforms_preserve_winding() {
        for (name, mesh) in [("player_wide", player_mesh()), ("player_slim", player_slim_mesh())] {
            for yaw in [0.0, 47.0, 90.0, 181.0, 300.0] {
                let anim = AnimInput {
                    limb_swing: yaw * 0.1,
                    limb_swing_amount: 1.0,
                    ..AnimInput::REST
                };
                let instance =
                    EntityInstance::new(name, &mesh, Vec3::new(3.0, 5.0, -2.0), yaw, 1.0, &anim);
                assert!(
                    !instance.part_transforms.is_empty(),
                    "{name}: expected a non-empty part chain"
                );
                for (i, part) in instance.part_transforms.iter().enumerate() {
                    assert!(
                        part.determinant() > 0.0,
                        "{name} part {i} at yaw {yaw}: determinant must be positive, was {}",
                        part.determinant()
                    );
                }
            }
        }
    }

    #[test]
    fn hand_projection_survives_a_degenerate_aspect() {
        assert!(hand_projection(0.0).to_cols_array().iter().all(|v| v.is_finite()));
        assert!(hand_projection(f32::NAN).to_cols_array().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_missing_left_hand_slot_falls_back_to_the_right_hand_one() {
        let right = DisplayTransform {
            rotation: [75.0, 45.0, 0.0],
            translation: [0.0, 2.5, 0.0],
            scale: [0.375, 0.375, 0.375],
        };
        let d = DisplayTransforms::NONE.with(DisplaySlot::ThirdPersonRightHand, right);
        assert_eq!(d.get(DisplaySlot::ThirdPersonLeftHand), right);
        assert_eq!(
            d.declared(DisplaySlot::ThirdPersonLeftHand),
            None,
            "the fallback must not pretend the slot was declared"
        );
        // A slot with no fallback rule still reads as the identity.
        assert_eq!(
            d.get(DisplaySlot::Ground),
            DisplayTransform::default(),
            "an undeclared non-hand slot is vanilla's NO_TRANSFORM"
        );
    }
}
