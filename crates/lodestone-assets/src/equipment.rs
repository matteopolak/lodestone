//! Humanoid armour: the four slot meshes, the **two inflations** they are baked
//! at, and the item → equipment-asset → texture chain that paints them.
//!
//! This is the data half of armour rendering. The render half lives in
//! `lodestone_render::entity` ([`ArmourModelSet`]) and the draw half in
//! `lodestone-shell`'s `gpu.rs`.
//!
//! # Why armour is a separate mesh set and not a corpus entry
//!
//! Every other drawable in this crate is an *entity* — a type path resolves to
//! one [`entity_models`](crate::entity_models) entry, which owns both its
//! geometry and its sheet. Armour is not an entity: it is a **layer over
//! somebody else's rig**, drawn from a mesh whose part names deliberately
//! collide with the wearer's so each piece can be posed by the wearer's own
//! already-animated part matrix. Registering it in the corpus would make
//! `canonical_model_name` able to resolve a helmet as if it were a mob.
//!
//! # The two inflations (this is the detail ports get wrong)
//!
//! Vanilla bakes the humanoid armour mesh set **twice**, at two different
//! `CubeDeformation`s, and hands the *inner* one to the legs slot only
//! (`LayerDefinitions.java:162-163`, `HumanoidModel.java:129-144`):
//!
//! ```text
//! OUTER_ARMOR_DEFORMATION = new CubeDeformation(1.0F)   // head, chest, feet
//! INNER_ARMOR_DEFORMATION = new CubeDeformation(0.5F)   // legs
//! ```
//!
//! A single-inflation port makes leggings clip through the chestplate that has
//! to sit outside them — leggings are the piece drawn *closest* to the body, and
//! the chestplate's `body` cube is drawn over the same torso at twice the grow.
//!
//! There are two further per-cube adjustments on top of the slot inflation, both
//! read from source rather than eyeballed:
//!
//! * **legs are `-0.1` texels thinner than their slot** — `createBaseArmorMesh`
//!   re-adds `right_leg`/`left_leg` with `g.extend(-0.1F)`
//!   (`HumanoidModel.java:146-160`, the constant is `LEGGINGS_OVERLAY_SCALE`).
//!   So the *effective* inflations are: head `1.0`, chest `1.0`, legs-slot legs
//!   `0.4`, legs-slot body `0.5`, feet `0.9`.
//! * **the helmet keeps `head`'s `hat` child at `+0.5`** — the head slot uses
//!   `retainPartsAndChildren({"head"})`, which retains a part *with its
//!   children*, and `hat` is authored at `g.extend(0.5F)`
//!   (`HumanoidModel.java:93`). Measured against 26.2's own sheets, the texels
//!   that cube unwraps onto (`x ∈ [32, 64)`, `y ∈ [0, 16)` of a 64×32 armour
//!   sheet) are **fully transparent in all nine humanoid armour textures**, so
//!   it contributes zero pixels — it is kept because vanilla keeps it, not
//!   because it draws.
//!
//! # Texture resolution
//!
//! An armour item does not name its texture. It carries
//! `minecraft:equippable`, whose `assetId` is a key into the
//! `equipment_asset` registry (`Equippable.java`, `ArmorMaterials.java`), and
//! the client reads `assets/<ns>/equipment/<asset>.json` for a per-**layer-type**
//! list of texture layers. `EquipmentClientInfo.Layer.getTextureLocation`
//! (`EquipmentClientInfo.java:105-107`) then builds
//! `textures/entity/equipment/<layer_type>/<texture>.png`.
//!
//! **`assetId` is not on the wire and not in the item-prototype census** (see
//! `docs/item-prototypes.md`: only `equippable.slot()` is carried), so the
//! item → asset mapping is a table here, transcribed from `ArmorMaterials`.
//! [`ARMOUR_ITEMS`] is that table and it is closed over 26.2: the
//! `humanoid_armour_items_cover_every_material` test walks it against
//! [`ARMOUR_ASSETS`].
//!
//! # Dye
//!
//! Only leather is dyeable. Its `humanoid`/`humanoid_leggings` layer lists are
//! two entries — a **greyscale** base layer that must be multiplied by a colour,
//! and an untinted `*_overlay` detail layer drawn over it. Measured on 26.2's
//! `humanoid/leather.png`: 589 of its 660 opaque texels are exactly grey, so a
//! port that skips the tint renders leather armour as pale iron.
//! [`UNDYED_LEATHER_RGB`] is `Dyeable.colorWhenUndyed` (`-6265536` in
//! `equipment/leather.json`, i.e. `0xFF_A0_65_40`) and is what
//! `EquipmentLayerRenderer.getColorForLayer` falls back to when the stack
//! carries no `minecraft:dyed_color`.
//!
//! The colour is **gamma-space sRGB bytes**, because that is the space vanilla
//! multiplies in: `submitModel(..., color, ...)` becomes a vertex colour that
//! multiplies the gamma-encoded texel byte-wise. Doing the multiply in linear
//! light pulls every factor toward `1.0` and washes the dye out — the same trap
//! `CLAUDE.md` records for tint and shade.

use crate::entity::{Deformation, EntityModelDef, PartDef};
use crate::entity_models::humanoid_root;

/// `LayerDefinitions.OUTER_ARMOR_DEFORMATION` — `new CubeDeformation(1.0F)`,
/// used for the head, chest and feet slots (`LayerDefinitions.java:162`).
pub const OUTER_ARMOUR_INFLATION: f32 = 1.0;

/// `LayerDefinitions.INNER_ARMOUR_DEFORMATION` — `new CubeDeformation(0.5F)`,
/// used for the **legs slot only** (`LayerDefinitions.java:163`).
///
/// This is the value a single-inflation port loses, and losing it is what makes
/// leggings clip through the chestplate.
pub const INNER_ARMOUR_INFLATION: f32 = 0.5;

/// `HumanoidModel.LEGGINGS_OVERLAY_SCALE` — the extra `-0.1F` every armour
/// mesh's *legs* carry relative to their slot inflation
/// (`HumanoidModel.java:33`, applied at `HumanoidModel.java:146-160`).
pub const LEGGINGS_OVERLAY_INFLATION: f32 = -0.1;

/// `HumanoidModel.HAT_OVERLAY_SCALE` — the helmet's `hat` child sits `+0.5`
/// texels outside the head cube (`HumanoidModel.java:32`, `:93`).
pub const HAT_OVERLAY_INFLATION: f32 = 0.5;

/// Armour sheets are **64×32**, not the 64×64 a modern player skin uses
/// (`LayerDefinitions.java:174` — `LayerDefinition.create(mesh, 64, 32)`).
///
/// Getting this wrong halves every V coordinate and paints the legs with the
/// head's pixels, which looks like a texture-resolution bug rather than a mesh
/// one.
pub const ARMOUR_SHEET_WIDTH: u32 = 64;

/// Armour sheet height — see [`ARMOUR_SHEET_WIDTH`].
pub const ARMOUR_SHEET_HEIGHT: u32 = 32;

/// `Dyeable.colorWhenUndyed` for leather, as gamma-space sRGB bytes.
///
/// `equipment/leather.json` declares `-6265536` = `0xFF_A0_65_40`;
/// `ARGB::opaque` forces the alpha, leaving `(160, 101, 64)`.
pub const UNDYED_LEATHER_RGB: [u8; 3] = [0xA0, 0x65, 0x40];

/// The four humanoid armour slots, i.e. exactly
/// `EquipmentSlot.Type.HUMANOID_ARMOR` (`EquipmentSlot.java:15-19`).
///
/// **`Body` and `Saddle` are not here and must not be folded in.** `BODY` is
/// `ANIMAL_ARMOR` (wolf armour, horse armour) and `SADDLE` is its own type;
/// vanilla's humanoid-armour gate is the `HUMANOID_ARMOR` type test, and a fold
/// of `"body"` into `Chest` would put a horse's barding on a player's torso.
/// See `docs/item-prototypes.md` for the census-side version of the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmourSlot {
    /// Helmet.
    Head,
    /// Chestplate.
    Chest,
    /// Leggings — the one slot drawn at [`INNER_ARMOUR_INFLATION`].
    Legs,
    /// Boots.
    Feet,
}

impl ArmourSlot {
    /// All four slots, in `HumanoidArmorLayer.submit`'s own draw order
    /// (chest, legs, feet, head — `HumanoidArmorLayer.java:48-52`).
    ///
    /// The order matters for coplanar layers: it is the order vanilla submits
    /// in, so a renderer that walks this array draws in vanilla's sequence.
    pub const ALL: [ArmourSlot; 4] = [
        ArmourSlot::Chest,
        ArmourSlot::Legs,
        ArmourSlot::Feet,
        ArmourSlot::Head,
    ];

    /// The slot's `CubeDeformation`: [`INNER_ARMOUR_INFLATION`] for
    /// [`Legs`](Self::Legs), [`OUTER_ARMOUR_INFLATION`] for the rest.
    ///
    /// `HumanoidModel.createArmorMeshSet` (`HumanoidModel.java:129-144`) picks
    /// `innerDeformation` for the legs mesh and `outerDeformation` for head,
    /// chest and feet.
    #[must_use]
    pub const fn inflation(self) -> f32 {
        match self {
            ArmourSlot::Legs => INNER_ARMOUR_INFLATION,
            _ => OUTER_ARMOUR_INFLATION,
        }
    }

    /// Which texture layer type paints this slot.
    ///
    /// `HumanoidArmorLayer.usesInnerModel` is `slot == LEGS`, and the layer type
    /// follows it: leggings read `humanoid_leggings`, everything else `humanoid`
    /// (`HumanoidArmorLayer.java:66-74`). Baby rigs use a third type,
    /// `humanoid_baby`, which this crate does not model — see
    /// [`ArmourLayerType`].
    #[must_use]
    pub const fn layer_type(self) -> ArmourLayerType {
        match self {
            ArmourSlot::Legs => ArmourLayerType::HumanoidLeggings,
            _ => ArmourLayerType::Humanoid,
        }
    }

    /// The wearer part names this slot's mesh carries geometry on.
    ///
    /// These are *the wearer's* part names, not private ones: an armour piece is
    /// posed by looking each name up in the wearer's own skeleton and reusing
    /// that part's already-animated matrix, so the names have to collide.
    ///
    /// Transcribed from `HumanoidModel.ADULT_ARMOR_PARTS_PER_SLOT`
    /// (`HumanoidModel.java:44-54`), with `head`'s `hat` child added because the
    /// head slot uses `retainPartsAndChildren` rather than `retainExactParts`.
    #[must_use]
    pub const fn part_names(self) -> &'static [&'static str] {
        match self {
            ArmourSlot::Head => &["head", "hat"],
            ArmourSlot::Chest => &["body", "right_arm", "left_arm"],
            ArmourSlot::Legs => &["body", "right_leg", "left_leg"],
            ArmourSlot::Feet => &["right_leg", "left_leg"],
        }
    }
}

/// An `EquipmentClientInfo.LayerType` — the sub-directory an equipment texture
/// lives in and the key its layer list is stored under
/// (`EquipmentClientInfo.java:109-128`).
///
/// Only the two humanoid-armour types are modelled. `humanoid_baby` (a
/// completely different mesh, `createBabyArmorMesh`), `wings` (elytra),
/// `wolf_body`/`horse_body`/`llama_body` (animal armour) and the eleven saddle
/// types are all separate layers with their own models, not variants of this
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmourLayerType {
    /// `humanoid` — head, chest and feet.
    Humanoid,
    /// `humanoid_leggings` — the legs slot.
    HumanoidLeggings,
}

impl ArmourLayerType {
    /// The serialized name, which is also the texture sub-directory.
    #[must_use]
    pub const fn serialized_name(self) -> &'static str {
        match self {
            ArmourLayerType::Humanoid => "humanoid",
            ArmourLayerType::HumanoidLeggings => "humanoid_leggings",
        }
    }
}

/// One `EquipmentClientInfo.Layer`: a texture name plus, if the layer is
/// dyeable, the colour to use when the stack carries no dye.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmourLayer {
    /// The layer's texture name within its layer-type directory (the `texture`
    /// field of `equipment/<asset>.json`, namespace stripped).
    pub texture: &'static str,
    /// `Dyeable.colorWhenUndyed` as gamma-space sRGB bytes, or `None` for a
    /// layer with no `dyeable` block at all.
    ///
    /// `EquipmentLayerRenderer.getColorForLayer` returns `-1` (white, i.e. no
    /// tint) for a non-dyeable layer and `dyeColor != 0 ? dyeColor :
    /// colorWhenUndyed` for a dyeable one. A stack's own
    /// `minecraft:dyed_color` overrides this, and that component *is* decoded
    /// through to the renderer, so this is the fallback for an undyed piece
    /// rather than the only colour leather can draw with.
    pub dye: Option<[u8; 3]>,
}

/// One `equipment_asset` registry entry, as read from
/// `assets/minecraft/equipment/<id>.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmourAsset {
    /// The asset id (`minecraft:` namespace stripped), e.g. `"diamond"`.
    pub id: &'static str,
    /// The `humanoid` layer list, in draw order.
    pub humanoid: &'static [ArmourLayer],
    /// The `humanoid_leggings` layer list, in draw order. Empty when the asset
    /// declares none — `turtle_scute` is the real case, and `getLayers` returns
    /// `List.of()` for it rather than falling back to `humanoid`.
    pub humanoid_leggings: &'static [ArmourLayer],
}

impl ArmourAsset {
    /// The layer list for a layer type, empty when the asset declares none.
    #[must_use]
    pub const fn layers(&self, layer_type: ArmourLayerType) -> &'static [ArmourLayer] {
        match layer_type {
            ArmourLayerType::Humanoid => self.humanoid,
            ArmourLayerType::HumanoidLeggings => self.humanoid_leggings,
        }
    }
}

/// The two-layer leather list: a greyscale dyeable base, then an untinted
/// detail overlay. Shared by both humanoid layer types, exactly as
/// `equipment/leather.json` declares it.
const LEATHER_LAYERS: &[ArmourLayer] = &[
    ArmourLayer {
        texture: "leather",
        dye: Some(UNDYED_LEATHER_RGB),
    },
    ArmourLayer {
        texture: "leather_overlay",
        dye: None,
    },
];

const COPPER_LAYERS: &[ArmourLayer] = &[ArmourLayer {
    texture: "copper",
    dye: None,
}];
const CHAINMAIL_LAYERS: &[ArmourLayer] = &[ArmourLayer {
    texture: "chainmail",
    dye: None,
}];
const IRON_LAYERS: &[ArmourLayer] = &[ArmourLayer {
    texture: "iron",
    dye: None,
}];
const GOLD_LAYERS: &[ArmourLayer] = &[ArmourLayer {
    texture: "gold",
    dye: None,
}];
const DIAMOND_LAYERS: &[ArmourLayer] = &[ArmourLayer {
    texture: "diamond",
    dye: None,
}];
const NETHERITE_LAYERS: &[ArmourLayer] = &[ArmourLayer {
    texture: "netherite",
    dye: None,
}];
const TURTLE_SCUTE_LAYERS: &[ArmourLayer] = &[ArmourLayer {
    texture: "turtle_scute",
    dye: None,
}];

/// Every `equipment_asset` a **humanoid** armour item can name in 26.2,
/// transcribed from `assets/minecraft/equipment/*.json` in `client.jar`.
///
/// The animal-armour and saddle assets (`armadillo_scute`, `*_carpet`,
/// `*_harness`, `saddle`, `trader_llama*`) and `elytra` are deliberately absent:
/// none of them declares a `humanoid` layer, and none of their items sits in a
/// `HUMANOID_ARMOR` slot.
pub const ARMOUR_ASSETS: &[ArmourAsset] = &[
    ArmourAsset {
        id: "leather",
        humanoid: LEATHER_LAYERS,
        humanoid_leggings: LEATHER_LAYERS,
    },
    ArmourAsset {
        id: "copper",
        humanoid: COPPER_LAYERS,
        humanoid_leggings: COPPER_LAYERS,
    },
    ArmourAsset {
        id: "chainmail",
        humanoid: CHAINMAIL_LAYERS,
        humanoid_leggings: CHAINMAIL_LAYERS,
    },
    ArmourAsset {
        id: "iron",
        humanoid: IRON_LAYERS,
        humanoid_leggings: IRON_LAYERS,
    },
    ArmourAsset {
        id: "gold",
        humanoid: GOLD_LAYERS,
        humanoid_leggings: GOLD_LAYERS,
    },
    ArmourAsset {
        id: "diamond",
        humanoid: DIAMOND_LAYERS,
        humanoid_leggings: DIAMOND_LAYERS,
    },
    ArmourAsset {
        id: "netherite",
        humanoid: NETHERITE_LAYERS,
        humanoid_leggings: NETHERITE_LAYERS,
    },
    ArmourAsset {
        id: "turtle_scute",
        // `turtle_scute.json` declares `humanoid` and `humanoid_baby` only —
        // there is no turtle leggings item, so there is no leggings layer.
        humanoid: TURTLE_SCUTE_LAYERS,
        humanoid_leggings: &[],
    },
];

/// `(item path, slot, equipment asset id)` for every item vanilla draws through
/// `HumanoidArmorLayer` in 26.2.
///
/// # Why this is a table and not derived
///
/// The mapping is `Item` → `Equippable.assetId()`, set in `ArmorMaterials`
/// (`ArmorMaterials.java`) and baked into the item's prototype component map.
/// It is never sent — a clientbound `/give diamond_helmet` arrives with an
/// **empty** component patch — and the committed prototype census carries only
/// `equippable.slot()`, not the asset id (`docs/item-prototypes.md`, "Only the
/// slot is carried"). So there is nothing to derive it from.
///
/// # Why every humanoid-slot item is *not* here
///
/// 26.2 has 38 items in a `HUMANOID_ARMOR` slot; only these 29 have an
/// `assetId`, and `HumanoidArmorLayer.shouldRender` requires one
/// (`HumanoidArmorLayer.java:38-45`). The other nine are drawn by other layers
/// entirely: `carved_pumpkin` and the seven skulls by `CustomHeadLayer` (a block
/// or skull model on the head, not an armour mesh), and `elytra` by `WingsLayer`
/// with its own `ElytraModel`. Listing them here would draw a *helmet-shaped*
/// pumpkin.
pub const ARMOUR_ITEMS: &[(&str, ArmourSlot, &str)] = &[
    ("turtle_helmet", ArmourSlot::Head, "turtle_scute"),
    ("leather_helmet", ArmourSlot::Head, "leather"),
    ("leather_chestplate", ArmourSlot::Chest, "leather"),
    ("leather_leggings", ArmourSlot::Legs, "leather"),
    ("leather_boots", ArmourSlot::Feet, "leather"),
    ("copper_helmet", ArmourSlot::Head, "copper"),
    ("copper_chestplate", ArmourSlot::Chest, "copper"),
    ("copper_leggings", ArmourSlot::Legs, "copper"),
    ("copper_boots", ArmourSlot::Feet, "copper"),
    ("chainmail_helmet", ArmourSlot::Head, "chainmail"),
    ("chainmail_chestplate", ArmourSlot::Chest, "chainmail"),
    ("chainmail_leggings", ArmourSlot::Legs, "chainmail"),
    ("chainmail_boots", ArmourSlot::Feet, "chainmail"),
    ("iron_helmet", ArmourSlot::Head, "iron"),
    ("iron_chestplate", ArmourSlot::Chest, "iron"),
    ("iron_leggings", ArmourSlot::Legs, "iron"),
    ("iron_boots", ArmourSlot::Feet, "iron"),
    ("diamond_helmet", ArmourSlot::Head, "diamond"),
    ("diamond_chestplate", ArmourSlot::Chest, "diamond"),
    ("diamond_leggings", ArmourSlot::Legs, "diamond"),
    ("diamond_boots", ArmourSlot::Feet, "diamond"),
    // `golden_*`, not `gold_*`: the item is `golden_helmet` while the equipment
    // asset is `gold`. Deriving the asset id by stripping the piece suffix
    // would look up `equipment/golden.json`, which does not exist.
    ("golden_helmet", ArmourSlot::Head, "gold"),
    ("golden_chestplate", ArmourSlot::Chest, "gold"),
    ("golden_leggings", ArmourSlot::Legs, "gold"),
    ("golden_boots", ArmourSlot::Feet, "gold"),
    ("netherite_helmet", ArmourSlot::Head, "netherite"),
    ("netherite_chestplate", ArmourSlot::Chest, "netherite"),
    ("netherite_leggings", ArmourSlot::Legs, "netherite"),
    ("netherite_boots", ArmourSlot::Feet, "netherite"),
];

/// The `equipment_asset` entry with this id, or `None`.
#[must_use]
pub fn armour_asset(id: &str) -> Option<&'static ArmourAsset> {
    ARMOUR_ASSETS.iter().find(|a| a.id == id)
}

/// The `(slot, asset)` a `minecraft:`-namespaced item path resolves to, or
/// `None` for an item this layer does not draw.
///
/// The slot returned is the item's *own* declared slot. A caller should compare
/// it against the slot the server actually put the item in and draw only on a
/// match — that is `HumanoidArmorLayer.shouldRender`'s `equippable.slot() ==
/// slot` test (`HumanoidArmorLayer.java:42-44`), and it is what stops a helmet
/// dropped into a boots slot by a plugin from rendering as a boot.
#[must_use]
pub fn armour_item(item_path: &str) -> Option<(ArmourSlot, &'static ArmourAsset)> {
    ARMOUR_ITEMS
        .iter()
        .find(|(path, _, _)| *path == item_path)
        .and_then(|(_, slot, asset)| armour_asset(asset).map(|a| (*slot, a)))
}

/// The in-jar texture path for one layer of one layer type — vanilla's
/// `EquipmentClientInfo.Layer.getTextureLocation`
/// (`EquipmentClientInfo.java:105-107`), with the namespace fixed to
/// `minecraft` because every layer in [`ARMOUR_ASSETS`] is unnamespaced.
#[must_use]
pub fn armour_texture_path(layer: &ArmourLayer, layer_type: ArmourLayerType) -> String {
    format!(
        "assets/minecraft/textures/entity/equipment/{}/{}.png",
        layer_type.serialized_name(),
        layer.texture
    )
}

/// Clear the cubes of every child not named in `keep`, recursing into the ones
/// that were cleared — vanilla's `PartDefinition.retainPartsAndChildren`
/// (`PartDefinition.java:55-62`).
///
/// A retained part keeps **its whole subtree**, which is the difference from
/// [`retain_exact`] and the reason a helmet carries `hat`.
fn retain_with_children(part: &mut PartDef, keep: &[&str]) {
    for (name, child) in &mut part.children {
        if !keep.contains(&name.as_str()) {
            child.cubes.clear();
            retain_with_children(child, keep);
        }
    }
}

/// Clear the cubes of every child not named in `keep`, and clear the entire
/// subtree of the ones that *are* — vanilla's
/// `PartDefinition.retainExactParts` (`PartDefinition.java:64-73`), where a
/// retained part is `clearRecursively()`d so only its own cubes survive.
fn retain_exact(part: &mut PartDef, keep: &[&str]) {
    for (name, child) in &mut part.children {
        if keep.contains(&name.as_str()) {
            clear_subtree(child);
        } else {
            child.cubes.clear();
            retain_exact(child, keep);
        }
    }
}

/// Empty every descendant's cube list, keeping this part's own — vanilla's
/// `PartDefinition.clearRecursively` (`PartDefinition.java:37-43`).
fn clear_subtree(part: &mut PartDef) {
    for (_, child) in &mut part.children {
        child.cubes.clear();
        clear_subtree(child);
    }
}

/// `HumanoidModel.createBaseArmorMesh(g)` (`HumanoidModel.java:146-160`): the
/// shared humanoid mesh at inflation `g`, with **both legs re-added at
/// `g.extend(-0.1)`**.
///
/// The leg override is not cosmetic. Without it the boots' leg cubes and the
/// leggings' leg cubes sit at exactly the slot inflation, and boots (outer,
/// `1.0`) then swallow the leggings (inner, `0.5`) wherever the two overlap at
/// the ankle. `-0.1` is `LEGGINGS_OVERLAY_SCALE`.
fn base_armour_root(inflation: f32) -> PartDef {
    let mut root = humanoid_root(inflation);
    let leg_grow = Deformation::uniform(inflation + LEGGINGS_OVERLAY_INFLATION);
    for leg in ["right_leg", "left_leg"] {
        if let Some(part) = root.child_mut(leg) {
            for cube in &mut part.cubes {
                cube.grow = leg_grow;
            }
        }
    }
    root
}

/// The armour mesh for one slot: [`base_armour_root`] at the slot's own
/// [`inflation`](ArmourSlot::inflation), pruned to the slot's parts, on a
/// **64×32** sheet.
///
/// Mirrors `HumanoidModel.createArmorMeshSet` (`HumanoidModel.java:129-144`)
/// one slot at a time. The four results are the `ArmorModelSet` record vanilla
/// hands to `HumanoidArmorLayer`.
#[must_use]
pub fn humanoid_armour_model(slot: ArmourSlot) -> EntityModelDef {
    let mut root = base_armour_root(slot.inflation());
    match slot {
        // `retainPartsAndChildren`, so `head` keeps `hat`.
        ArmourSlot::Head => retain_with_children(&mut root, &["head"]),
        // `retainExactParts` for the other three.
        ArmourSlot::Chest => retain_exact(&mut root, &["body", "left_arm", "right_arm"]),
        ArmourSlot::Legs => retain_exact(&mut root, &["left_leg", "right_leg", "body"]),
        ArmourSlot::Feet => retain_exact(&mut root, &["left_leg", "right_leg"]),
    }
    EntityModelDef {
        texture_width: ARMOUR_SHEET_WIDTH,
        texture_height: ARMOUR_SHEET_HEIGHT,
        root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::bake_entity_parts;

    /// The two inflations, and the derived per-cube values. Pinned as numbers
    /// because they are the thing a port gets wrong, and because the *legs*
    /// slot differing from the other three is the whole point.
    #[test]
    fn the_two_inflations_are_one_half_and_one() {
        assert_eq!(ArmourSlot::Head.inflation(), 1.0);
        assert_eq!(ArmourSlot::Chest.inflation(), 1.0);
        assert_eq!(ArmourSlot::Feet.inflation(), 1.0);
        assert_eq!(ArmourSlot::Legs.inflation(), 0.5);
        assert!(
            ArmourSlot::Legs.inflation() < ArmourSlot::Chest.inflation(),
            "leggings must be drawn INSIDE the chestplate; equal inflations are \
             the single-inflation port bug"
        );
    }

    /// Every slot's baked mesh must carry geometry on exactly the parts
    /// `ADULT_ARMOR_PARTS_PER_SLOT` names and on nothing else. A retention bug
    /// is invisible in a screenshot of a fully-armoured mob (some other piece
    /// covers the leak) and obvious in a mob wearing one piece.
    #[test]
    fn each_slot_carries_geometry_on_exactly_its_own_parts() {
        for slot in ArmourSlot::ALL {
            let baked = bake_entity_parts(&humanoid_armour_model(slot));
            let with_quads: Vec<&str> = baked
                .iter()
                .filter(|p| !p.quads.is_empty())
                .map(|p| p.name.as_str())
                .collect();
            let mut expected = slot.part_names().to_vec();
            expected.sort_unstable();
            let mut got = with_quads.clone();
            got.sort_unstable();
            assert_eq!(got, expected, "{slot:?} draws on the wrong parts");
        }
    }

    /// The helmet is the only slot with a `hat` cube, and the head slot is the
    /// only one that uses `retainPartsAndChildren`. If someone "simplifies"
    /// the head arm to `retain_exact`, `hat` silently vanishes.
    #[test]
    fn only_the_helmet_retains_the_hat_child() {
        for slot in ArmourSlot::ALL {
            let baked = bake_entity_parts(&humanoid_armour_model(slot));
            let hat = baked
                .iter()
                .find(|p| p.name == "hat")
                .expect("every armour mesh keeps the hat node, cubes or not");
            assert_eq!(
                hat.quads.is_empty(),
                slot != ArmourSlot::Head,
                "{slot:?}: hat geometry present == (slot is Head) violated"
            );
        }
    }

    /// The legs of every slot are `0.1` texels thinner than the slot's own
    /// inflation, and the body/arms/head are not. Measured on the *baked* box
    /// extents rather than on the `CubeDef`, so a change that drops the
    /// override anywhere between here and the bake is caught.
    #[test]
    fn legs_are_a_tenth_of_a_texel_thinner_than_their_slot() {
        // The base humanoid leg box is 4 wide, so half-width at inflation `g`
        // is `(4 + 2g) / 2 / 16` blocks.
        let half_width = |g: f32| (4.0 + 2.0 * g) / 32.0;
        for slot in [ArmourSlot::Legs, ArmourSlot::Feet] {
            let baked = bake_entity_parts(&humanoid_armour_model(slot));
            let leg = baked
                .iter()
                .find(|p| p.name == "right_leg" && !p.quads.is_empty())
                .expect("leg geometry");
            let max_x = leg
                .quads
                .iter()
                .flat_map(|q| q.positions.iter().map(|p| p[0]))
                .fold(f32::NEG_INFINITY, f32::max);
            let expected = half_width(slot.inflation() + LEGGINGS_OVERLAY_INFLATION);
            assert!(
                (max_x - expected).abs() < 1e-5,
                "{slot:?} leg half-width {max_x} != {expected} \
                 (slot inflation {} plus the -0.1 leggings override)",
                slot.inflation()
            );
        }
        // The chest's body cube gets the plain slot inflation, no override.
        let baked = bake_entity_parts(&humanoid_armour_model(ArmourSlot::Chest));
        let body = baked
            .iter()
            .find(|p| p.name == "body" && !p.quads.is_empty())
            .expect("body geometry");
        let max_x = body
            .quads
            .iter()
            .flat_map(|q| q.positions.iter().map(|p| p[0]))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (max_x - (8.0 + 2.0 * OUTER_ARMOUR_INFLATION) / 32.0).abs() < 1e-5,
            "the chestplate's body cube must NOT carry the leggings override"
        );
    }

    /// A legs-slot piece must sit strictly inside the chest-slot piece on the
    /// torso, because both slots draw a `body` cube over the same ribs. This is
    /// the assertion that fails on a single-inflation port, and it is stated on
    /// the geometry rather than on the constants so it survives a refactor.
    #[test]
    fn the_leggings_body_cube_sits_inside_the_chestplate_body_cube() {
        let extent = |slot: ArmourSlot| {
            let baked = bake_entity_parts(&humanoid_armour_model(slot));
            let body = baked
                .iter()
                .find(|p| p.name == "body" && !p.quads.is_empty())
                .expect("body geometry");
            body.quads
                .iter()
                .flat_map(|q| q.positions.iter().map(|p| p[2]))
                .fold(f32::NEG_INFINITY, f32::max)
        };
        let inner = extent(ArmourSlot::Legs);
        let outer = extent(ArmourSlot::Chest);
        assert!(
            inner < outer,
            "leggings torso depth {inner} must be inside chestplate {outer}"
        );
    }

    /// The sheet is 64×32. A 64×64 assumption halves every V and paints the
    /// legs with the helmet's pixels.
    #[test]
    fn armour_sheets_are_sixty_four_by_thirty_two() {
        for slot in ArmourSlot::ALL {
            let def = humanoid_armour_model(slot);
            assert_eq!((def.texture_width, def.texture_height), (64, 32));
        }
    }

    /// Every item in the table resolves to an asset that exists, declares the
    /// layer type its slot needs, and reports the slot the item's own name
    /// implies. Also: every asset is reachable from at least one item, so a
    /// dead asset entry cannot hide here.
    #[test]
    fn humanoid_armour_items_cover_every_material() {
        assert_eq!(ARMOUR_ITEMS.len(), 29, "26.2 has 29 humanoid armour items");
        for (path, slot, asset_id) in ARMOUR_ITEMS {
            let (resolved_slot, asset) =
                armour_item(path).unwrap_or_else(|| panic!("{path} must resolve"));
            assert_eq!(resolved_slot, *slot);
            assert_eq!(asset.id, *asset_id);
            let expected_suffix = match slot {
                ArmourSlot::Head => "helmet",
                ArmourSlot::Chest => "chestplate",
                ArmourSlot::Legs => "leggings",
                ArmourSlot::Feet => "boots",
            };
            assert!(
                path.ends_with(expected_suffix),
                "{path} is in the {slot:?} slot but is not a {expected_suffix}"
            );
            assert!(
                !asset.layers(slot.layer_type()).is_empty(),
                "{path} needs {:?} layers and {} declares none",
                slot.layer_type(),
                asset.id
            );
        }
        for asset in ARMOUR_ASSETS {
            assert!(
                ARMOUR_ITEMS.iter().any(|(_, _, id)| *id == asset.id),
                "{} is unreachable from any item",
                asset.id
            );
        }
    }

    /// The nine humanoid-slot items vanilla draws through some *other* layer
    /// must not resolve here. A pumpkin rendered as a helmet mesh is a
    /// plausible-looking wrong build.
    #[test]
    fn non_armour_head_items_do_not_resolve() {
        for path in [
            "carved_pumpkin",
            "elytra",
            "skeleton_skull",
            "wither_skeleton_skull",
            "player_head",
            "zombie_head",
            "creeper_head",
            "dragon_head",
            "piglin_head",
        ] {
            assert!(
                armour_item(path).is_none(),
                "{path} is drawn by CustomHeadLayer/WingsLayer, not HumanoidArmorLayer"
            );
        }
    }

    /// Leather is the only dyeable material, its base layer is dyeable and its
    /// overlay is not, and the undyed colour is vanilla's.
    #[test]
    fn only_leather_is_dyeable_and_only_its_base_layer() {
        for asset in ARMOUR_ASSETS {
            for layer_type in [ArmourLayerType::Humanoid, ArmourLayerType::HumanoidLeggings] {
                for (i, layer) in asset.layers(layer_type).iter().enumerate() {
                    let dyeable = layer.dye.is_some();
                    assert_eq!(
                        dyeable,
                        asset.id == "leather" && i == 0,
                        "{}/{:?} layer {i} ({}) dyeable == {dyeable}",
                        asset.id,
                        layer_type,
                        layer.texture
                    );
                }
            }
        }
        assert_eq!(UNDYED_LEATHER_RGB, [160, 101, 64]);
        // 0xFFA06540, the `color_when_undyed: -6265536` in
        // `equipment/leather.json` with ARGB::opaque applied.
        let argb = i32::from_be_bytes([
            0xFF,
            UNDYED_LEATHER_RGB[0],
            UNDYED_LEATHER_RGB[1],
            UNDYED_LEATHER_RGB[2],
        ]);
        assert_eq!(argb, -6_265_536);
    }

    /// Texture paths must land where 26.2's `client.jar` actually keeps them.
    #[test]
    fn texture_paths_match_the_jar_layout() {
        let leather = armour_asset("leather").expect("leather");
        assert_eq!(
            armour_texture_path(&leather.humanoid[0], ArmourLayerType::Humanoid),
            "assets/minecraft/textures/entity/equipment/humanoid/leather.png"
        );
        assert_eq!(
            armour_texture_path(&leather.humanoid[1], ArmourLayerType::Humanoid),
            "assets/minecraft/textures/entity/equipment/humanoid/leather_overlay.png"
        );
        assert_eq!(
            armour_texture_path(
                &leather.humanoid_leggings[0],
                ArmourLayerType::HumanoidLeggings
            ),
            "assets/minecraft/textures/entity/equipment/humanoid_leggings/leather.png"
        );
    }

    /// The armour mesh's part names must be the *wearer's* part names, since
    /// each piece is posed off the wearer's matrix for the same name. Checked
    /// against the real corpus humanoid rather than against a copy of the list.
    #[test]
    fn armour_part_names_exist_on_the_corpus_humanoid() {
        let wearer = bake_entity_parts(&crate::entity_models::zombie_model());
        for slot in ArmourSlot::ALL {
            for name in slot.part_names() {
                assert!(
                    wearer.iter().any(|p| p.name == *name),
                    "{slot:?} wants wearer part {name:?}, which the humanoid rig lacks"
                );
            }
        }
    }
}
