//! The hand-ported entity-model corpus for the 1.21.5–26.2 family.
//!
//! Entity geometry is **code, not data** in vanilla (see [`crate::entity`]), so
//! each mesh here is transcribed by hand from the decompiled client's own
//! per-mob model classes. This module holds only the *data* (the
//! per-mob [`EntityModelDef`]s and their default texture paths); the version-free
//! baking primitive lives in [`crate::entity`]. In the project's ideal shape this
//! data would live in a version crate the way `AssetProfile` is supplied per
//! version; it lives here for now alongside `player_model`, clearly scoped to the
//! modern family, and the loader/baker never branches on version.
//!
//! Meshes are largely stable across versions, so this is authored once and
//! tweaked per version rather than reimplemented. Texel offsets, box extents,
//! poses and sheet sizes are the exact vanilla values; the sheet size of every
//! entry is checked against the real texture PNG in `client.jar` by the
//! `real_jar` coverage test, so a mistranscribed sheet size cannot pass silently.

use crate::entity::{
    CatCoat, CubeDef, EntityModelDef, EntityTexture, EntityVariant, HorseColor, HorseMarkings,
    LlamaColor, MooshroomColor, ParrotColor, PartDef, PartPose, Temperature, WolfCoat, WolfState,
    player_model,
};
use std::f32::consts::PI;

/// One ported entity model: a stable `name`, its texture (fixed or
/// variant-driven, see [`EntityTexture`]), and a builder that produces the
/// bake-ready [`EntityModelDef`].
///
/// 26.2 split pig/cow/chicken into `_temperate`/`_cold`/`_warm` variants and
/// removed the bare `pig.png`, so those entries carry an [`EntityTexture::ByVariant`]
/// selector with the temperate skin as the canonical default; invariant mobs are
/// [`EntityTexture::Fixed`].
#[derive(Clone, Debug)]
pub struct EntityModelEntry {
    /// Stable identifier for the model (not necessarily the registry id).
    pub name: &'static str,
    /// The texture sheet(s) for this model, resolved via [`EntityTexture`].
    pub texture: EntityTexture,
    /// Builds the bake-ready model.
    pub build: fn() -> EntityModelDef,
}

/// `_temperate`/`_cold`/`_warm` selector for a mob whose only variant axis is
/// climate (pig, cow, chicken in 26.2). Each mob gets a named selector below
/// because a `fn` pointer cannot capture per-mob path literals.
fn pig_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Temperature(Temperature::Cold) => "entity/pig/pig_cold",
        EntityVariant::Temperature(Temperature::Warm) => "entity/pig/pig_warm",
        EntityVariant::Temperature(Temperature::Temperate) => "entity/pig/pig_temperate",
        // `EntityVariant` grew axes for other mobs (horse colour, llama, cat,
        // wolf, parrot); pig only cares about `Temperature`, so it falls
        // through to its own canonical default for all of them.
        _ => "entity/pig/pig_temperate",
    }
}

fn cow_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Temperature(Temperature::Cold) => "entity/cow/cow_cold",
        EntityVariant::Temperature(Temperature::Warm) => "entity/cow/cow_warm",
        EntityVariant::Temperature(Temperature::Temperate) => "entity/cow/cow_temperate",
        _ => "entity/cow/cow_temperate",
    }
}

fn chicken_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Temperature(Temperature::Cold) => "entity/chicken/chicken_cold",
        EntityVariant::Temperature(Temperature::Warm) => "entity/chicken/chicken_warm",
        EntityVariant::Temperature(Temperature::Temperate) => "entity/chicken/chicken_temperate",
        _ => "entity/chicken/chicken_temperate",
    }
}

fn cube(origin: [f32; 3], size: [f32; 3], tex: [f32; 2]) -> CubeDef {
    CubeDef::new(origin, size, tex)
}

/// The shared humanoid mesh (vanilla's own base humanoid-mesh construction, `g`, y-offset 0): head with
/// a hat overlay, body, two arms and two legs, on the standard box layout. `g` is
/// the uniform cube deformation (`0.0` for the base layer).
///
/// Visible to the crate because [`crate::equipment`] builds the armour layers
/// from the *same* function at `g = 0.5` / `g = 1.0`. Vanilla does exactly that
/// — vanilla's own humanoid model's base-armor-mesh construction calls `createMesh(g, 0.0F)` — and
/// sharing it is what keeps an armour piece's pivots identical to the wearer's,
/// which is the precondition for posing a piece off the wearer's own part
/// matrix.
pub(crate) fn humanoid_root(g: f32) -> PartDef {
    let head = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]).grown(g))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [32.0, 0.0]).grown(g + 0.5)),
        );
    PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
                .with_cube(cube([-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0]).grown(g)),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, 2.0, 0.0))
                .with_cube(cube([-3.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 16.0]).grown(g)),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, 2.0, 0.0)).with_cube(
                cube([-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 16.0])
                    .grown(g)
                    .mirrored(),
            ),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-1.9, 12.0, 0.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0]).grown(g)),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(1.9, 12.0, 0.0)).with_cube(
                cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0])
                    .grown(g)
                    .mirrored(),
            ),
        )
}


/// The shared quadruped mesh (vanilla's own quadruped-model body-mesh construction): head, rotated
/// body, and four legs. `mirror_left`/`mirror_right` mirror the respective legs.
fn quadruped_root(leg_size: f32, mirror_left: bool, mirror_right: bool) -> PartDef {
    let right = || {
        let c = cube([-2.0, 0.0, -2.0], [4.0, leg_size, 4.0], [0.0, 16.0]);
        if mirror_right { c.mirrored() } else { c }
    };
    let left = || {
        let c = cube([-2.0, 0.0, -2.0], [4.0, leg_size, 4.0], [0.0, 16.0]);
        if mirror_left { c.mirrored() } else { c }
    };
    PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 18.0 - leg_size, -6.0)).with_cube(cube(
                [-4.0, -4.0, -8.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                17.0 - leg_size,
                2.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-5.0, -10.0, -7.0], [10.0, 16.0, 8.0], [28.0, 8.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 24.0 - leg_size, 7.0)).with_cube(right()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.0, 24.0 - leg_size, 7.0)).with_cube(left()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.0, 24.0 - leg_size, -5.0)).with_cube(right()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(3.0, 24.0 - leg_size, -5.0)).with_cube(left()),
        )
}


mod basic_humanoids;
mod quadrupeds;
mod monsters;
mod special;
mod vehicles;
mod creatures;
mod equines_felines;
mod misc;

pub use basic_humanoids::*;
pub use quadrupeds::*;
pub use monsters::*;
pub use special::*;
pub use vehicles::*;
pub use creatures::*;
pub use equines_felines::*;
pub use misc::*;

use equines_felines::{cat_coat_texture, horse_color_texture, llama_color_texture, parrot_color_texture, wolf_coat_texture};
use misc::mooshroom_color_texture;
use monsters::scaled;

fn player_wide() -> EntityModelDef {
    player_model(false)
}

fn player_slim() -> EntityModelDef {
    player_model(true)
}

/// The ported entity-model corpus, in a fixed order (priority: player first,
/// then the common overworld set). Growing this list is how coverage climbs.
pub fn entity_models() -> Vec<EntityModelEntry> {
    vec![
        EntityModelEntry {
            name: "player_wide",
            texture: EntityTexture::Fixed("entity/player/wide/steve"),
            build: player_wide,
        },
        EntityModelEntry {
            name: "player_slim",
            texture: EntityTexture::Fixed("entity/player/slim/alex"),
            build: player_slim,
        },
        EntityModelEntry {
            name: "zombie",
            texture: EntityTexture::Fixed("entity/zombie/zombie"),
            build: zombie_model,
        },
        EntityModelEntry {
            name: "skeleton",
            texture: EntityTexture::Fixed("entity/skeleton/skeleton"),
            build: skeleton_model,
        },
        EntityModelEntry {
            name: "creeper",
            texture: EntityTexture::Fixed("entity/creeper/creeper"),
            build: creeper_model,
        },
        EntityModelEntry {
            name: "spider",
            texture: EntityTexture::Fixed("entity/spider/spider"),
            build: spider_model,
        },
        EntityModelEntry {
            name: "pig",
            texture: EntityTexture::ByVariant {
                default: "entity/pig/pig_temperate",
                select: pig_texture,
            },
            build: pig_model,
        },
        EntityModelEntry {
            name: "cow",
            texture: EntityTexture::ByVariant {
                default: "entity/cow/cow_temperate",
                select: cow_texture,
            },
            build: cow_model,
        },
        EntityModelEntry {
            name: "sheep",
            texture: EntityTexture::Fixed("entity/sheep/sheep"),
            build: sheep_model,
        },
        EntityModelEntry {
            name: "chicken",
            texture: EntityTexture::ByVariant {
                default: "entity/chicken/chicken_temperate",
                select: chicken_texture,
            },
            build: chicken_model,
        },
        // Tier 2: common overworld/hostile expansion. Texture-only variants reuse
        // an existing builder because vanilla renders them with the same model
        // class. Where vanilla applies a mesh-transformer scaling to the
        // layer (baked into the mesh, not the renderer), we wrap the base builder
        // in `scaled(..)` so the geometry carries the same size.
        EntityModelEntry {
            name: "husk",
            texture: EntityTexture::Fixed("entity/zombie/husk"),
            build: husk_model,
        },
        EntityModelEntry {
            name: "stray",
            texture: EntityTexture::Fixed("entity/skeleton/stray"),
            build: skeleton_model,
        },
        EntityModelEntry {
            name: "wither_skeleton",
            texture: EntityTexture::Fixed("entity/skeleton/wither_skeleton"),
            build: wither_skeleton_model,
        },
        EntityModelEntry {
            name: "cave_spider",
            texture: EntityTexture::Fixed("entity/spider/cave_spider"),
            build: cave_spider_model,
        },
        EntityModelEntry {
            name: "slime",
            texture: EntityTexture::Fixed("entity/slime/slime"),
            build: slime_model,
        },
        EntityModelEntry {
            name: "magma_cube",
            texture: EntityTexture::Fixed("entity/slime/magmacube"),
            build: magma_cube_model,
        },
        EntityModelEntry {
            name: "blaze",
            texture: EntityTexture::Fixed("entity/blaze/blaze"),
            build: blaze_model,
        },
        EntityModelEntry {
            name: "squid",
            texture: EntityTexture::Fixed("entity/squid/squid"),
            build: squid_model,
        },
        EntityModelEntry {
            name: "bat",
            texture: EntityTexture::Fixed("entity/bat/bat"),
            build: bat_model,
        },
        EntityModelEntry {
            name: "enderman",
            texture: EntityTexture::Fixed("entity/enderman/enderman"),
            build: enderman_model,
        },
        // Tier 3: monster/* remainder (impl-assets lane).
        EntityModelEntry {
            name: "drowned",
            texture: EntityTexture::Fixed("entity/zombie/drowned"),
            build: drowned_model,
        },
        EntityModelEntry {
            name: "iron_golem",
            texture: EntityTexture::Fixed("entity/iron_golem/iron_golem"),
            build: iron_golem_model,
        },
        EntityModelEntry {
            name: "snow_golem",
            texture: EntityTexture::Fixed("entity/snow_golem/snow_golem"),
            build: snow_golem_model,
        },
        EntityModelEntry {
            name: "vex",
            texture: EntityTexture::Fixed("entity/illager/vex"),
            build: vex_model,
        },
        EntityModelEntry {
            name: "silverfish",
            texture: EntityTexture::Fixed("entity/silverfish/silverfish"),
            build: silverfish_model,
        },
        EntityModelEntry {
            name: "endermite",
            texture: EntityTexture::Fixed("entity/endermite/endermite"),
            build: endermite_model,
        },
        EntityModelEntry {
            name: "piglin",
            texture: EntityTexture::Fixed("entity/piglin/piglin"),
            build: piglin_model,
        },
        EntityModelEntry {
            name: "zombified_piglin",
            texture: EntityTexture::Fixed("entity/piglin/zombified_piglin"),
            build: piglin_model,
        },
        EntityModelEntry {
            name: "piglin_brute",
            texture: EntityTexture::Fixed("entity/piglin/piglin_brute"),
            build: piglin_model,
        },
        EntityModelEntry {
            name: "ghast",
            texture: EntityTexture::Fixed("entity/ghast/ghast"),
            build: ghast_model,
        },
        EntityModelEntry {
            name: "hoglin",
            texture: EntityTexture::Fixed("entity/hoglin/hoglin"),
            build: hoglin_model,
        },
        EntityModelEntry {
            name: "zoglin",
            texture: EntityTexture::Fixed("entity/hoglin/zoglin"),
            build: hoglin_model,
        },
        EntityModelEntry {
            name: "strider",
            texture: EntityTexture::Fixed("entity/strider/strider"),
            build: strider_model,
        },
        EntityModelEntry {
            name: "guardian",
            texture: EntityTexture::Fixed("entity/guardian/guardian"),
            build: guardian_model,
        },
        EntityModelEntry {
            name: "phantom",
            texture: EntityTexture::Fixed("entity/phantom/phantom"),
            build: phantom_model,
        },
        EntityModelEntry {
            name: "warden",
            texture: EntityTexture::Fixed("entity/warden/warden"),
            build: warden_model,
        },
        EntityModelEntry {
            name: "wither",
            texture: EntityTexture::Fixed("entity/wither/wither"),
            build: wither_model,
        },
        EntityModelEntry {
            name: "ender_dragon",
            texture: EntityTexture::Fixed("entity/enderdragon/dragon"),
            build: ender_dragon_model,
        },
        EntityModelEntry {
            name: "witch",
            texture: EntityTexture::Fixed("entity/witch/witch"),
            build: witch_model,
        },
        EntityModelEntry {
            name: "villager",
            texture: EntityTexture::Fixed("entity/villager/villager"),
            build: villager_model,
        },
        EntityModelEntry {
            name: "zombie_villager",
            texture: EntityTexture::Fixed("entity/zombie_villager/zombie_villager"),
            build: zombie_villager_model,
        },
        // --- animal/npc/object half (owned by this agent). Horse family,
        // cat/wolf/ocelot and parrot are deferred pending the texture-variant
        // seam (see the module banner above `end_crystal_model`). item_frame
        // is intentionally absent: vanilla resolves it via a block-model JSON
        // (vanilla's own item-frame renderer/block-model resolver), not a `ModelPart`
        // `LayerDefinition`, so it does not fit `CubeDef`'s single-tex_offset
        // box-unwrap primitive without extending it or routing through the
        // block-model pipeline instead. ---
        EntityModelEntry {
            name: "armor_stand",
            texture: EntityTexture::Fixed("entity/armorstand/armorstand"),
            build: armor_stand_model,
        },
        EntityModelEntry {
            name: "boat",
            texture: EntityTexture::Fixed("entity/boat/oak"),
            build: boat_model,
        },
        EntityModelEntry {
            name: "chest_boat",
            texture: EntityTexture::Fixed("entity/chest_boat/oak"),
            build: chest_boat_model,
        },
        // The invisible water-clip mask both `boat` and `chest_boat` draw a
        // second, separately-pipelined instance of — see
        // `boat_water_patch_model`'s own doc. The texture is real (so this
        // loads exactly like every other corpus entry) but never sampled into
        // the framebuffer: `EntityPipeline::water_mask_pipeline` disables
        // colour writes.
        EntityModelEntry {
            name: "boat_water_patch",
            texture: EntityTexture::Fixed("entity/boat/oak"),
            build: boat_water_patch_model,
        },
        EntityModelEntry {
            name: "raft",
            texture: EntityTexture::Fixed("entity/boat/bamboo"),
            build: raft_model,
        },
        EntityModelEntry {
            name: "chest_raft",
            texture: EntityTexture::Fixed("entity/chest_boat/bamboo"),
            build: chest_raft_model,
        },
        EntityModelEntry {
            name: "minecart",
            texture: EntityTexture::Fixed("entity/minecart/minecart"),
            build: minecart_model,
        },
        EntityModelEntry {
            name: "end_crystal",
            texture: EntityTexture::Fixed("entity/end_crystal/end_crystal"),
            build: end_crystal_model,
        },
        EntityModelEntry {
            name: "rabbit",
            texture: EntityTexture::Fixed("entity/rabbit/rabbit_brown"),
            build: rabbit_model,
        },
        EntityModelEntry {
            name: "fox",
            texture: EntityTexture::Fixed("entity/fox/fox"),
            build: fox_model,
        },
        EntityModelEntry {
            name: "panda",
            texture: EntityTexture::Fixed("entity/panda/panda"),
            build: panda_model,
        },
        EntityModelEntry {
            name: "goat",
            texture: EntityTexture::Fixed("entity/goat/goat"),
            build: goat_model,
        },
        EntityModelEntry {
            name: "bee",
            texture: EntityTexture::Fixed("entity/bee/bee"),
            build: bee_model,
        },
        EntityModelEntry {
            name: "turtle",
            texture: EntityTexture::Fixed("entity/turtle/turtle"),
            build: turtle_model,
        },
        EntityModelEntry {
            name: "camel",
            texture: EntityTexture::Fixed("entity/camel/camel"),
            build: camel_model,
        },
        EntityModelEntry {
            name: "cod",
            texture: EntityTexture::Fixed("entity/fish/cod"),
            build: cod_model,
        },
        EntityModelEntry {
            name: "salmon",
            texture: EntityTexture::Fixed("entity/fish/salmon"),
            build: salmon_model,
        },
        EntityModelEntry {
            name: "pufferfish",
            texture: EntityTexture::Fixed("entity/fish/pufferfish"),
            build: pufferfish_model,
        },
        EntityModelEntry {
            name: "tropical_fish",
            // vanilla's own tropical-fish renderer pairs its own large
            // tropical-fish model (this
            // entry's geometry, chosen to avoid the small model's
            // negative texOffs) with `tropical_b.png`, not `tropical_a.png`
            // (that's the small model's texture) — confirmed directly against
            // vanilla's own tropical-fish-renderer texture-location query.
            texture: EntityTexture::Fixed("entity/fish/tropical_b"),
            build: tropical_fish_model,
        },
        EntityModelEntry {
            name: "dolphin",
            texture: EntityTexture::Fixed("entity/dolphin/dolphin"),
            build: dolphin_model,
        },
        EntityModelEntry {
            name: "axolotl",
            texture: EntityTexture::Fixed("entity/axolotl/axolotl_lucy"),
            build: axolotl_model,
        },
        EntityModelEntry {
            name: "frog",
            texture: EntityTexture::Fixed("entity/frog/frog_temperate"),
            build: frog_model,
        },
        EntityModelEntry {
            name: "tadpole",
            texture: EntityTexture::Fixed("entity/tadpole/tadpole"),
            build: tadpole_model,
        },
        EntityModelEntry {
            name: "sniffer",
            texture: EntityTexture::Fixed("entity/sniffer/sniffer"),
            build: sniffer_model,
        },
        EntityModelEntry {
            name: "armadillo",
            texture: EntityTexture::Fixed("entity/armadillo/armadillo"),
            build: armadillo_model,
        },
        // ---- horse family, cat/wolf/ocelot, parrot: variant-driven, see the
        // module banner above `equine_base_root` for the horse-markings caveat ----
        EntityModelEntry {
            name: "horse",
            texture: EntityTexture::ByVariant {
                default: "entity/horse/horse_white",
                select: horse_color_texture,
            },
            build: horse_model,
        },
        EntityModelEntry {
            name: "donkey",
            texture: EntityTexture::Fixed("entity/horse/donkey"),
            build: donkey_model,
        },
        EntityModelEntry {
            name: "mule",
            texture: EntityTexture::Fixed("entity/horse/mule"),
            build: mule_model,
        },
        EntityModelEntry {
            name: "skeleton_horse",
            texture: EntityTexture::Fixed("entity/horse/horse_skeleton"),
            build: skeleton_horse_model,
        },
        EntityModelEntry {
            name: "zombie_horse",
            texture: EntityTexture::Fixed("entity/horse/horse_zombie"),
            build: zombie_horse_model,
        },
        EntityModelEntry {
            name: "llama",
            texture: EntityTexture::ByVariant {
                default: "entity/llama/llama_creamy",
                select: llama_color_texture,
            },
            build: llama_model,
        },
        EntityModelEntry {
            name: "trader_llama",
            texture: EntityTexture::ByVariant {
                default: "entity/llama/llama_creamy",
                select: llama_color_texture,
            },
            build: trader_llama_model,
        },
        EntityModelEntry {
            name: "cat",
            texture: EntityTexture::ByVariant {
                default: "entity/cat/cat_tabby",
                select: cat_coat_texture,
            },
            build: cat_model,
        },
        EntityModelEntry {
            name: "ocelot",
            texture: EntityTexture::Fixed("entity/cat/ocelot"),
            build: ocelot_model,
        },
        EntityModelEntry {
            name: "wolf",
            texture: EntityTexture::ByVariant {
                default: "entity/wolf/wolf",
                select: wolf_coat_texture,
            },
            build: wolf_model,
        },
        EntityModelEntry {
            name: "parrot",
            texture: EntityTexture::ByVariant {
                default: "entity/parrot/parrot_red_blue",
                select: parrot_color_texture,
            },
            build: parrot_model,
        },
        // ---- second overworld-priority batch: polar_bear, illager raid
        // roster, ravager, allay, shulker ----
        EntityModelEntry {
            name: "polar_bear",
            texture: EntityTexture::Fixed("entity/bear/polarbear"),
            build: polar_bear_model,
        },
        EntityModelEntry {
            name: "pillager",
            texture: EntityTexture::Fixed("entity/illager/pillager"),
            build: pillager_model,
        },
        EntityModelEntry {
            name: "vindicator",
            texture: EntityTexture::Fixed("entity/illager/vindicator"),
            build: vindicator_model,
        },
        EntityModelEntry {
            name: "evoker",
            texture: EntityTexture::Fixed("entity/illager/evoker"),
            build: evoker_model,
        },
        EntityModelEntry {
            name: "illusioner",
            texture: EntityTexture::Fixed("entity/illager/illusioner"),
            build: illusioner_model,
        },
        EntityModelEntry {
            name: "ravager",
            texture: EntityTexture::Fixed("entity/illager/ravager"),
            build: ravager_model,
        },
        EntityModelEntry {
            name: "allay",
            texture: EntityTexture::Fixed("entity/allay/allay"),
            build: allay_model,
        },
        EntityModelEntry {
            name: "shulker",
            texture: EntityTexture::Fixed("entity/shulker/shulker"),
            build: shulker_model,
        },
        // ---- cheap-reuse mobs: existing builder, new registry entry only ----
        EntityModelEntry {
            name: "glow_squid",
            texture: EntityTexture::Fixed("entity/squid/glow_squid"),
            build: glow_squid_model,
        },
        EntityModelEntry {
            name: "wandering_trader",
            texture: EntityTexture::Fixed("entity/wandering_trader/wandering_trader"),
            build: wandering_trader_model,
        },
        EntityModelEntry {
            name: "mooshroom",
            texture: EntityTexture::ByVariant {
                default: "entity/cow/mooshroom_red",
                select: mooshroom_color_texture,
            },
            build: mooshroom_model,
        },
        // ---- the "invisible but solid" set: types the hitbox table already
        // knew about while the rig corpus did not, so a player collided with
        // something that drew nothing ----
        // ---- projectiles and effects with a cuboid rig of their own: placed by
        // `non_living_vehicle_matrix` or `projectile_model_matrix`, never by the
        // mob placement, because none of their renderers is a living-entity one ----
        EntityModelEntry {
            name: "evoker_fangs",
            texture: EntityTexture::Fixed("entity/illager/evoker_fangs"),
            build: evoker_fangs_model,
        },
        EntityModelEntry {
            name: "shulker_bullet",
            texture: EntityTexture::Fixed("entity/shulker/spark"),
            build: shulker_bullet_model,
        },
        EntityModelEntry {
            name: "wither_skull",
            // The harmless sheet; `wither_invulnerable` is chosen per entity by
            // a bit this rig has no channel for.
            texture: EntityTexture::Fixed("entity/wither/wither"),
            build: wither_skull_model,
        },
        EntityModelEntry {
            name: "llama_spit",
            texture: EntityTexture::Fixed("entity/llama/llama_spit"),
            build: llama_spit_model,
        },
        // `breeze_wind_charge` shares this rig via an alias in
        // `canonical_model_name_for_type` rather than a second entry here — see
        // `wind_charge_model`'s doc.
        EntityModelEntry {
            name: "wind_charge",
            texture: EntityTexture::Fixed("entity/projectiles/wind_charge"),
            build: wind_charge_model,
        },
        EntityModelEntry {
            name: "elder_guardian",
            texture: EntityTexture::Fixed("entity/guardian/guardian_elder"),
            build: elder_guardian_model,
        },
        EntityModelEntry {
            name: "parched",
            texture: EntityTexture::Fixed("entity/skeleton/parched"),
            build: parched_model,
        },
        EntityModelEntry {
            name: "giant",
            // The giant reuses the zombie's sheet outright; only the mesh scale
            // differs, so there is no `giant.png` to point at.
            texture: EntityTexture::Fixed("entity/zombie/zombie"),
            build: giant_model,
        },
        EntityModelEntry {
            name: "leash_knot",
            texture: EntityTexture::Fixed("entity/lead_knot/lead_knot"),
            build: leash_knot_model,
        },
        EntityModelEntry {
            name: "sulfur_cube",
            // The adult shell. The `_small` sheet belongs to the size-1 rig,
            // which is a separate baked layer this corpus does not carry.
            texture: EntityTexture::Fixed("entity/sulfur_cube/sulfur_cube_outer"),
            build: sulfur_cube_model,
        },
        EntityModelEntry {
            name: "breeze",
            texture: EntityTexture::Fixed("entity/breeze/breeze"),
            build: breeze_model,
        },
        EntityModelEntry {
            name: "creaking",
            texture: EntityTexture::Fixed("entity/creaking/creaking"),
            build: creaking_model,
        },
        EntityModelEntry {
            name: "copper_golem",
            // The unoxidised sheet; the other three stages are a per-entity
            // state axis nothing on this side carries yet.
            texture: EntityTexture::Fixed("entity/copper_golem/copper_golem"),
            build: copper_golem_model,
        },
        EntityModelEntry {
            name: "happy_ghast",
            texture: EntityTexture::Fixed("entity/ghast/happy_ghast"),
            build: happy_ghast_model,
        },
        EntityModelEntry {
            name: "nautilus",
            texture: EntityTexture::Fixed("entity/nautilus/nautilus"),
            build: nautilus_model,
        },
        EntityModelEntry {
            name: "zombie_nautilus",
            // Same rig, own sheet — a sibling of `nautilus`, not a variant of
            // it, because vanilla picks it by renderer class rather than by
            // entity state. The coral crust is a second baked layer and is not
            // part of this mesh.
            texture: EntityTexture::Fixed("entity/nautilus/zombie_nautilus"),
            build: nautilus_model,
        },
        EntityModelEntry {
            name: "camel_husk",
            // The camel rig, unmodified, on its own sheet — the same
            // one-rig-two-sheets shape as `nautilus`/`zombie_nautilus`.
            texture: EntityTexture::Fixed("entity/camel/camel_husk"),
            build: camel_model,
        },
        // ---- projectiles: placed by `projectile_model_matrix`, not by
        // `entity_model_matrix` (see the "Projectiles" section above) ----
        EntityModelEntry {
            name: "arrow",
            // Vanilla's own tippable-arrow-renderer normal-arrow-location constant. The tipped sheet
            // (`arrow_tipped`) is a second, potion-colour-driven texture chosen
            // by `state.isTipped`; that bit is not decoded here, so an
            // arrow-of-harming draws as a plain arrow rather than as a wrongly
            // *tinted* one.
            texture: EntityTexture::Fixed("entity/projectiles/arrow"),
            build: arrow_model,
        },
        EntityModelEntry {
            name: "spectral_arrow",
            // Vanilla's own spectral-arrow-renderer spectral-arrow-location constant. Same rig, own
            // sheet — a sibling of `arrow`, not a variant of it, because vanilla
            // picks it by *renderer class* rather than by entity state.
            texture: EntityTexture::Fixed("entity/projectiles/arrow_spectral"),
            build: arrow_model,
        },
        EntityModelEntry {
            name: "trident",
            texture: EntityTexture::Fixed("entity/trident/trident"),
            build: trident_model,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corpus really carries the water-clip mask, not just the standalone
    /// builder function — the island shape this repo's own defects keep
    /// taking: a function can be correct and unit-testable while nothing
    /// wires it into the table `EntityModelSet::load()` actually walks.
    #[test]
    fn the_water_patch_is_a_real_corpus_entry() {
        let entry = entity_models()
            .into_iter()
            .find(|e| e.name == "boat_water_patch")
            .expect("boat_water_patch must be registered in entity_models()");
        assert_eq!((entry.build)(), boat_water_patch_model());
    }

    /// **Both hypotheses, from the real vanilla source.** vanilla's own boat model's water-patch construction
    /// (`.cache/mc/26.2/client-src`) builds the *same* box `boat_hull`'s own
    /// `"bottom"` child does — `addBox(-14, -9, -3, 28, 16, 3)` at `texOffs(0, 0)`
    /// — but at pose `offsetAndRotation(0, -3, 1, PI/2, 0, 0)`, where `"bottom"`
    /// sits at `offsetAndRotation(0, 3, 1, PI/2, 0, 0)`. Only the pivot's `y`
    /// differs, and only in sign: everything else — the box, the rotation, `x`,
    /// `z` — must be bit-identical, or the patch sits somewhere vanilla's own
    /// hollow-interior fix does not, and the gap it exists to close reopens on
    /// one side.
    #[test]
    fn the_water_patch_mirrors_the_hulls_own_bottom_plank() {
        let hull = vehicles::boat_hull();
        let bottom = hull
            .children
            .iter()
            .find(|(name, _)| name == "bottom")
            .map(|(_, part)| part)
            .expect("boat_hull must carry a \"bottom\" child");

        let patch_model = boat_water_patch_model();
        assert_eq!(patch_model.texture_width, 128, "same sheet width as the boat itself");
        assert_eq!(patch_model.texture_height, 64, "same sheet height as the boat itself");
        let (name, patch) = &patch_model.root.children[0];
        assert_eq!(name, "water_patch");

        // The one axis that must differ, and only in sign: this is the whole
        // mechanism by which the patch sits *inside* the hollow rather than on
        // the visible outer hull.
        assert!(
            (patch.pose.y - (-bottom.pose.y)).abs() < 1e-6,
            "the patch's y pivot ({}) must be the *negation* of bottom's ({}), not a copy",
            patch.pose.y,
            bottom.pose.y
        );
        // The wrong-but-plausible neighbour: a straight copy of "bottom",
        // which would draw the mask exactly where the visible hull already is
        // and leave the actual gap (further up, inside the hollow) unmasked.
        assert!(
            (patch.pose.y - bottom.pose.y).abs() > 1.0,
            "the patch must not merely copy bottom's pose, or it masks the wrong plane"
        );

        // Every other pose axis: identical.
        assert!((patch.pose.x - bottom.pose.x).abs() < 1e-6, "x pivot must match");
        assert!((patch.pose.z - bottom.pose.z).abs() < 1e-6, "z pivot must match");
        assert!((patch.pose.x_rot - bottom.pose.x_rot).abs() < 1e-6, "x rotation must match");
        assert!((patch.pose.y_rot - bottom.pose.y_rot).abs() < 1e-6, "y rotation must match");
        assert!((patch.pose.z_rot - bottom.pose.z_rot).abs() < 1e-6, "z rotation must match");

        // The box itself: bit-identical origin/size/tex_offset to `"bottom"`'s,
        // per vanilla's own boat model's water-patch construction's own `texOffs(0, 0).addBox(-14, -9,
        // -3, 28, 16, 3)` — the same literal `addBox` call `addCommonParts`
        // makes for `"bottom"`.
        assert_eq!(patch.cubes.len(), 1, "the patch is one box, not the whole hull");
        assert_eq!(bottom.cubes.len(), 1);
        assert_eq!(patch.cubes[0].origin, bottom.cubes[0].origin, "box origin must match");
        assert_eq!(patch.cubes[0].size, bottom.cubes[0].size, "box size must match");
        assert_eq!(
            patch.cubes[0].tex_offset,
            bottom.cubes[0].tex_offset,
            "tex offset must match (irrelevant for colour, but a real transcription \
             checks the whole literal, not just the parts that render differently)"
        );
    }

    /// The negative control this pair needs: `chest_boat_model` must **not**
    /// carry its own water-patch child — vanilla's own boat renderer submits the
    /// mask as a second model entirely, not folded into either boat variant's
    /// own part tree (see `boat_water_patch_model`'s doc for why it is a
    /// separate corpus entry).
    #[test]
    fn neither_boat_variant_carries_the_patch_as_its_own_child() {
        for (label, model) in [("boat", boat_model()), ("chest_boat", chest_boat_model())] {
            assert!(
                !model.root.children.iter().any(|(name, _)| name == "water_patch"),
                "{label} must not carry \"water_patch\" as a child part"
            );
        }
    }
}
