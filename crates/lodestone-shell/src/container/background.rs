//! Vanilla's real `container/*.png` panel art, stitched into one small atlas.
//!
//! Split out of `container.rs` verbatim.

use lodestone_assets::gui::{GuiMeta, GuiScaling};
use lodestone_assets::{Atlas, AtlasBuilder, AtlasError, ResourceLocation, ResourceManager};
use lodestone_game::menu::{Menu, MenuKind, SpecialLayout};
use lodestone_render::GuiSpriteQuad;

#[cfg(doc)]
use super::GUI_SPRITES;

/// The five per-tab advancement background ids, exactly as the datapack's
/// `display.background` writes them.
pub(crate) const ADVANCEMENT_TILE_IDS: [&str; 5] = [
    "minecraft:gui/advancements/backgrounds/stone",
    "minecraft:gui/advancements/backgrounds/nether",
    "minecraft:gui/advancements/backgrounds/end",
    "minecraft:gui/advancements/backgrounds/adventure",
    "minecraft:gui/advancements/backgrounds/husbandry",
];

/// A GUI sprite id (`container/slot/helmet`) as the texture location it lives at
/// (`minecraft:gui/sprites/container/slot/helmet`).
///
/// Vanilla's `blitSprite` resolves sprite ids through the GUI sprite atlas,
/// whose sources are `gui/sprites/**`; this client has no sprite-atlas indirection
/// so the prefix is applied here.
fn sprite_location(id: &str) -> Option<ResourceLocation> {
    ResourceLocation::new("minecraft", &format!("gui/sprites/{id}")).ok()
}

/// The sprite ids whose `gui.scaling` this module records at build time.
///
/// Everything else this atlas stitches is blitted at (or sampled from) its
/// native size, where `Stretch` and the declared mode agree; these two are
/// drawn at a caller-chosen width and would smear their border without the
/// real nine-slice decomposition.
const NINE_SLICE_SPRITES: &[&str] = &[
    crate::effects::EFFECT_BACKGROUND_SPRITE,
    crate::effects::EFFECT_BACKGROUND_AMBIENT_SPRITE,
];

/// `assets/<ns>/textures/mob_effect/<name>.png` → `(ns, name)`.
///
/// Anything else — including a `.mcmeta`, or a nested subdirectory vanilla's
/// own `directory` atlas source would flatten differently — yields `None`.
fn split_mob_effect_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("assets/")?;
    let (namespace, rest) = rest.split_once('/')?;
    let name = rest.strip_prefix("textures/mob_effect/")?.strip_suffix(".png")?;
    if namespace.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((namespace, name))
}

/// Vanilla's real container-background art (issue #51): `container/inventory`,
/// `container/crafting_table` and `container/generic_54`, stitched into one
/// small atlas.
///
/// Reproduced by hand rather than through
/// [`lodestone_render::GuiAtlas`](lodestone_render::GuiAtlas): these three PNGs
/// live at `textures/gui/container/**`, not `textures/gui/sprites/**`, so they
/// carry no sibling `.mcmeta` and vanilla does not scale them through any of
/// [`lodestone_assets::gui::GuiScaling`]'s three modes. Instead it blits
/// hand-placed sub-rectangles of each 256×256 sheet at native size —
/// `ContainerScreen.java` draws the chest background as *two* blits (the
/// row-count-dependent top part, then a fixed 96 px bottom part immediately
/// below it), `CraftingScreen.java` and `InventoryScreen.java` each
/// draw one whole-panel blit. `GuiScaling` has no variant for an arbitrary
/// sub-rect, so this reads the sheets' atlas placement directly and computes
/// the same UV windows vanilla's `blit` calls use, rather than forcing the
/// three-mode abstraction to do something it was never built for.
///
/// Deliberately GPU-free (mirrors [`lodestone_render::GuiAtlas`]'s own
/// producer/consumer split): [`ContainerBackground::build`] is the producer,
/// [`ContainerBackground::quads`] the pure consumer a test can call with no
/// device.
#[derive(Debug)]
pub struct ContainerBackground {
    atlas: Atlas,
    generic: ResourceLocation,
    crafting: ResourceLocation,
    inventory: ResourceLocation,
    anvil: ResourceLocation,
    grindstone: ResourceLocation,
    smithing: ResourceLocation,
    enchantment: ResourceLocation,
    furnace: ResourceLocation,
    blast_furnace: ResourceLocation,
    smoker: ResourceLocation,
    brewing_stand: ResourceLocation,
    loom: ResourceLocation,
    stonecutter: ResourceLocation,
    cartography_table: ResourceLocation,
    /// Shared by the dispenser **and** the dropper — see
    /// [`SpecialLayout::Dispenser`]'s doc comment.
    dispenser: ResourceLocation,
    hopper: ResourceLocation,
    /// `textures/gui/container/villager.png` (issue #245's UI half) — a
    /// `512×256` sheet, not `256×256` like every sheet above; the atlas
    /// placement is unaffected (see [`Self::quads`]'s `whole_panel_sized`),
    /// only the sub-rect grabbed from it (`276×166`) differs.
    merchant: ResourceLocation,
    /// `textures/gui/container/beacon.png` (issue #613's `SetBeaconEffects`
    /// remainder) — a `256×256` sheet like every non-merchant one above, but
    /// a taller-than-usual `230×219` whole-panel blit
    /// (`BeaconScreen.java`'s `super(menu, inventory, title, 230, 219)`).
    beacon: ResourceLocation,
    /// The creative screen's three sheets (issue #158) — `tab_items`,
    /// `tab_item_search`, `tab_inventory`. Same family as every sheet above:
    /// loose `textures/gui/container/**` art with no `.mcmeta` and no
    /// `GuiScaling`, blitted at native size. See
    /// [`Self::creative_quad`].
    creative_items: ResourceLocation,
    /// See [`Self::creative_items`](Self::creative_quad).
    creative_search: ResourceLocation,
    /// See [`Self::creative_items`](Self::creative_quad).
    creative_inventory: ResourceLocation,
    /// The Advancements screen's window art (issue #167) —
    /// `textures/gui/advancements/window.png`, another loose sheet blitted at a
    /// sub-rect (`252 x 140` of `256 x 256`).
    advancements_window: ResourceLocation,
    /// The five per-tab tiled backgrounds, keyed by their datapack id
    /// (`minecraft:gui/advancements/backgrounds/stone`, ...). Each is a real
    /// `16 x 16` texture — measured, not assumed from
    /// `BACKGROUND_TILE_WIDTH`.
    advancements_tiles: Vec<(&'static str, ResourceLocation)>,
    /// The status-effect icons, keyed by the sprite id
    /// `Hud.getMobEffectSprite` builds (`mob_effect/<path>`).
    ///
    /// These are **not** `gui/sprites/**` art, which is why nothing here found
    /// them before: `assets/minecraft/atlases/gui.json` declares a *second*
    /// source directory for the GUI atlas —
    /// `{"type": "directory", "prefix": "mob_effect/", "source": "mob_effect"}`
    /// — so the file is `textures/mob_effect/<path>.png` while the sprite id
    /// still reads like an ordinary atlas entry. Any enumeration that assumes
    /// one source directory misses all 41 of them, and a widget that blits one
    /// draws nothing.
    ///
    /// Enumerated from the pack rather than transcribed as a list, so a
    /// resource pack (or a datapack-added effect) contributes its own icon.
    mob_effect_icons: Vec<(String, ResourceLocation)>,
    /// The `gui.scaling` mode declared by a stitched `gui/sprites/**` sprite's
    /// sibling `.png.mcmeta`, keyed by sprite id — only recorded for the ids
    /// whose draw actually needs it (see [`Self::scaled_sprite_quads`]).
    ///
    /// Absent means [`GuiScaling::Stretch`], vanilla's own default for a
    /// sprite with no `.mcmeta`.
    sprite_scaling: Vec<(&'static str, GuiScaling)>,
}

/// Which vanilla `container/*.png` sheet a menu's background draws from, and
/// (for the generic-chest case) how many rows are actually shown — vanilla
/// truncates the top blit's height to `rows * 18 + 17` rather than always
/// drawing all six.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackgroundKind {
    Inventory,
    Crafting,
    Generic { rows: usize },
    Anvil,
    Grindstone,
    Smithing,
    Enchantment,
    Furnace,
    BlastFurnace,
    Smoker,
    Brewing,
    Loom,
    Stonecutter,
    Cartography,
    Dispenser,
    /// `176×133`, not `166` — see [`SpecialLayout::Hopper`]'s doc comment.
    Hopper,
    /// `276×166`, not `176×166` — see [`SpecialLayout::Merchant`]'s doc
    /// comment.
    Merchant,
    /// `230×219`, not `176×166` — see [`SpecialLayout::Beacon`]'s doc
    /// comment.
    Beacon,
}

/// Mirrors [`slot_layout`]'s own dispatch, **including** its
/// [`Menu::special_layout`] check (issues #253-#255, extended by #28 to the
/// furnace family, brewing stand, loom, stonecutter, cartography table and
/// dispenser/dropper): each of these gets its own real `container/*.png`
/// sheet, checked *before* the plain `MenuKind` dispatch for the same reason
/// `slot_layout` checks it before `craft_layout` — a menu with a
/// `special_layout` is mechanically a [`MenuKind::Generic`] and would
/// otherwise fall into the plain chest case. Everything else without one:
/// [`Menu::craft_layout`] draws the crafting table's background regardless of
/// container size (today always the 3×3 table), everything else generic draws
/// the chest sheet at its own row count, and [`MenuKind::Player`] draws the
/// player inventory sheet.
///
/// Every `special_layout` sheet but one is a single whole-panel `176×166`
/// blit at the sheet's origin, exactly like
/// [`BackgroundKind::Inventory`]/[`BackgroundKind::Crafting`]
/// (`AnvilScreen.java`-adjacent `blit` calls; every one of these screens'
/// `blit(texture, x, y, 0, 0, imageWidth, imageHeight)` uses the vanilla
/// `176×166` default, none override `imageWidth`/`imageHeight` — re-verified
/// against `AbstractContainerScreen.java`'s own default constructor for
/// the six added by #28, not merely assumed to match the first four). Three
/// exceptions pass a non-default size to their own `super(...)` constructor
/// and [`ContainerBackground::quads`] special-cases each rather than reusing
/// the `whole_panel` closure's hardcoded size: [`BackgroundKind::Hopper`]
/// (`176, 133`, `HopperScreen.java`), [`BackgroundKind::Merchant`] (`276,
/// 166`, `MerchantScreen.java`) and [`BackgroundKind::Beacon`] (`230, 219`,
/// `BeaconScreen.java`).
pub(super) fn background_kind(menu: &Menu) -> BackgroundKind {
    match menu.special_layout() {
        Some(SpecialLayout::Anvil) => return BackgroundKind::Anvil,
        Some(SpecialLayout::Grindstone) => return BackgroundKind::Grindstone,
        Some(SpecialLayout::Smithing) => return BackgroundKind::Smithing,
        Some(SpecialLayout::Enchanting) => return BackgroundKind::Enchantment,
        Some(SpecialLayout::Furnace) => return BackgroundKind::Furnace,
        Some(SpecialLayout::BlastFurnace) => return BackgroundKind::BlastFurnace,
        Some(SpecialLayout::Smoker) => return BackgroundKind::Smoker,
        Some(SpecialLayout::Brewing) => return BackgroundKind::Brewing,
        Some(SpecialLayout::Loom) => return BackgroundKind::Loom,
        Some(SpecialLayout::Stonecutter) => return BackgroundKind::Stonecutter,
        Some(SpecialLayout::Cartography) => return BackgroundKind::Cartography,
        Some(SpecialLayout::Dispenser) => return BackgroundKind::Dispenser,
        Some(SpecialLayout::Hopper) => return BackgroundKind::Hopper,
        Some(SpecialLayout::Merchant) => return BackgroundKind::Merchant,
        Some(SpecialLayout::Beacon) => return BackgroundKind::Beacon,
        None => {}
    }
    match menu.kind() {
        MenuKind::Player => BackgroundKind::Inventory,
        MenuKind::Generic { container_size } => match menu.craft_layout() {
            Some(_) => BackgroundKind::Crafting,
            None => BackgroundKind::Generic {
                rows: container_size.div_ceil(9).clamp(1, 6),
            },
        },
    }
}

impl ContainerBackground {
    /// Loads and stitches the sheets from a resource manager (in practice,
    /// `client.jar`).
    pub fn build(manager: &ResourceManager) -> Result<Self, AtlasError> {
        let generic = ResourceLocation::new("minecraft", "gui/container/generic_54")
            .expect("hardcoded location is always valid");
        let crafting = ResourceLocation::new("minecraft", "gui/container/crafting_table")
            .expect("hardcoded location is always valid");
        let inventory = ResourceLocation::new("minecraft", "gui/container/inventory")
            .expect("hardcoded location is always valid");
        // The four item-combiner-shaped screens (#253-#255): each is its own
        // whole-panel sheet, same family as the three above.
        let anvil = ResourceLocation::new("minecraft", "gui/container/anvil")
            .expect("hardcoded location is always valid");
        let grindstone = ResourceLocation::new("minecraft", "gui/container/grindstone")
            .expect("hardcoded location is always valid");
        let smithing = ResourceLocation::new("minecraft", "gui/container/smithing")
            .expect("hardcoded location is always valid");
        let enchantment = ResourceLocation::new("minecraft", "gui/container/enchanting_table")
            .expect("hardcoded location is always valid");
        // The six more added by issue #28: same family, one whole-panel sheet
        // each. `dispenser` is loaded once and shared by the dropper too —
        // see `SpecialLayout::Dispenser`'s doc comment for why there is no
        // separate `dropper` sheet to load.
        let furnace = ResourceLocation::new("minecraft", "gui/container/furnace")
            .expect("hardcoded location is always valid");
        let blast_furnace = ResourceLocation::new("minecraft", "gui/container/blast_furnace")
            .expect("hardcoded location is always valid");
        let smoker = ResourceLocation::new("minecraft", "gui/container/smoker")
            .expect("hardcoded location is always valid");
        let brewing_stand = ResourceLocation::new("minecraft", "gui/container/brewing_stand")
            .expect("hardcoded location is always valid");
        let loom = ResourceLocation::new("minecraft", "gui/container/loom")
            .expect("hardcoded location is always valid");
        let stonecutter = ResourceLocation::new("minecraft", "gui/container/stonecutter")
            .expect("hardcoded location is always valid");
        let cartography_table =
            ResourceLocation::new("minecraft", "gui/container/cartography_table")
                .expect("hardcoded location is always valid");
        let dispenser = ResourceLocation::new("minecraft", "gui/container/dispenser")
            .expect("hardcoded location is always valid");
        // Not one of #28's own named containers — found while writing this
        // list's doc comment (see `SpecialLayout::Hopper`).
        let hopper = ResourceLocation::new("minecraft", "gui/container/hopper")
            .expect("hardcoded location is always valid");
        // The merchant/trading screen (issue #245's UI half): a `512×256`
        // sheet, unlike every sheet above — see the `merchant` field's own doc
        // comment for why that needs no special handling here.
        let merchant = ResourceLocation::new("minecraft", "gui/container/villager")
            .expect("hardcoded location is always valid");
        let beacon = ResourceLocation::new("minecraft", "gui/container/beacon")
            .expect("hardcoded location is always valid");
        // The creative screen's three sheets (issue #158). Unlike the tab-button
        // and scroller art — which is `gui/sprites/**` and rides `GUI_SPRITES`
        // below — these are loose `gui/container/**` textures, so they need their
        // own locations exactly as the sixteen above do.
        let creative_items =
            ResourceLocation::new("minecraft", "gui/container/creative_inventory/tab_items")
                .expect("hardcoded location is always valid");
        let creative_search =
            ResourceLocation::new("minecraft", "gui/container/creative_inventory/tab_item_search")
                .expect("hardcoded location is always valid");
        let creative_inventory =
            ResourceLocation::new("minecraft", "gui/container/creative_inventory/tab_inventory")
                .expect("hardcoded location is always valid");
        // The Advancements screen's window plus its five tiled backgrounds
        // (issue #167). Same loose-`gui/**` family as everything above.
        let advancements_window = ResourceLocation::new("minecraft", "gui/advancements/window")
            .expect("hardcoded location is always valid");
        let advancements_tiles: Vec<(&'static str, ResourceLocation)> = ADVANCEMENT_TILE_IDS
            .iter()
            .map(|id| {
                let path = id.strip_prefix("minecraft:").unwrap_or(id);
                (
                    *id,
                    ResourceLocation::new("minecraft", path)
                        .expect("hardcoded location is always valid"),
                )
            })
            .collect();
        let mut builder = AtlasBuilder::new();
        builder.load(manager, &generic)?;
        builder.load(manager, &crafting)?;
        builder.load(manager, &inventory)?;
        builder.load(manager, &anvil)?;
        builder.load(manager, &grindstone)?;
        builder.load(manager, &smithing)?;
        builder.load(manager, &enchantment)?;
        builder.load(manager, &furnace)?;
        builder.load(manager, &blast_furnace)?;
        builder.load(manager, &smoker)?;
        builder.load(manager, &brewing_stand)?;
        builder.load(manager, &loom)?;
        builder.load(manager, &stonecutter)?;
        builder.load(manager, &cartography_table)?;
        builder.load(manager, &dispenser)?;
        builder.load(manager, &hopper)?;
        builder.load(manager, &merchant)?;
        builder.load(manager, &beacon)?;
        builder.load(manager, &creative_items)?;
        builder.load(manager, &creative_search)?;
        builder.load(manager, &creative_inventory)?;
        builder.load(manager, &advancements_window)?;
        for (_, loc) in &advancements_tiles {
            builder.load(manager, loc)?;
        }
        // The hover highlight and the empty-slot placeholders (issue #376) ride
        // in this same atlas rather than a second one. They are ordinary
        // textures with an ordinary `.png.mcmeta`, so `AtlasBuilder` needs no
        // new capability — and reusing the atlas means reusing the bind group
        // and pipeline `attach_background` already builds. A separate
        // `GuiAtlas` would have needed both, which is what
        // `tests/container_slot_sprites.rs` recorded as "a pipeline/bind-group
        // job"; it turned out not to be one.
        //
        // A missing sprite is a hard error here, matching the sheets above, so
        // a pack that drops one **names the sprite** instead of silently
        // drawing an empty cell.
        for id in super::all_gui_sprites() {
            let loc = sprite_location(id).ok_or_else(|| AtlasError::TextureMissing {
                location: id.to_string(),
            })?;
            builder.load(manager, &loc)?;
        }
        // The status-effect icons. Enumerated rather than listed: see the
        // `mob_effect_icons` field's doc for why they are not `gui/sprites/**`
        // and what that costs anyone who assumes they are.
        //
        // Fail-**open**, unlike the sprite loop above: these come from a
        // directory the pack is free to populate, so an undecodable file must
        // cost one icon, not the whole container atlas — and vanilla's own
        // `blitSprite` falls back to the missing-texture sprite per id.
        let mut mob_effect_icons: Vec<(String, ResourceLocation)> = Vec::new();
        for path in manager.list("assets/") {
            let Some((namespace, name)) = split_mob_effect_path(&path) else {
                continue;
            };
            let Ok(loc) = ResourceLocation::new(namespace, format!("mob_effect/{name}")) else {
                continue;
            };
            if mob_effect_icons.iter().any(|(id, _)| id == loc.path()) {
                continue;
            }
            if builder.load(manager, &loc).is_ok() {
                mob_effect_icons.push((loc.path().to_string(), loc));
            }
        }
        // Deterministic packing, matching `GuiAtlas::build_with_extras`' own
        // sort: the pack listing order is not guaranteed stable.
        mob_effect_icons.sort_by(|a, b| a.0.cmp(&b.0));
        // The two effect-panel backgrounds are `nine_slice` (a `32x32` sprite
        // with a `4` border, per their `.png.mcmeta`) and get drawn at an
        // arbitrary width, so — unlike every other sprite in this atlas —
        // stretching the whole sprite over the target is visibly wrong. Their
        // declared scaling is read from the pack here rather than transcribed,
        // for the same reason `GuiAtlas` reads it: a resource pack may change
        // the border.
        let mut sprite_scaling: Vec<(&'static str, GuiScaling)> = Vec::new();
        for id in NINE_SLICE_SPRITES {
            let Some(loc) = sprite_location(id) else {
                continue;
            };
            let meta_path = format!(
                "{}.mcmeta",
                ResourceManager::asset_path(&loc, "textures", "png")
            );
            let Some(bytes) = manager.read(&meta_path) else {
                continue;
            };
            if let Ok(meta) = GuiMeta::parse(&bytes) {
                sprite_scaling.push((id, meta.scaling));
            }
        }
        let atlas = builder.build()?;
        Ok(Self {
            atlas,
            generic,
            crafting,
            inventory,
            anvil,
            grindstone,
            smithing,
            enchantment,
            furnace,
            blast_furnace,
            smoker,
            brewing_stand,
            loom,
            stonecutter,
            cartography_table,
            dispenser,
            hopper,
            merchant,
            beacon,
            creative_items,
            creative_search,
            creative_inventory,
            advancements_window,
            advancements_tiles,
            mob_effect_icons,
            sprite_scaling,
        })
    }

    /// `AdvancementsScreen.BACKGROUND_TEXTURE_WIDTH`/`_HEIGHT` (`:34-35`) — the
    /// declared sheet size [`Self::advancements_window_quad`] scales its
    /// `252 x 140` sample against.
    const BACKGROUND_TEXTURE_SIZE: f32 = 256.0;

    /// The declared (16x-baseline) size of every whole-panel `gui/container/**`
    /// sheet [`Self::build`] stitches — `256×256` — except
    /// [`Self::merchant`]'s (see [`Self::MERCHANT_DECLARED`]).
    ///
    /// None of these sheets carries a `.mcmeta`, so a resource pack has no
    /// declared metadata to read for them the way a `gui/sprites/**` entry's
    /// nine-slice does — vanilla itself never scales its `blit` source rects
    /// by anything but this literal ratio against the sprite's real placed
    /// size (see [`Self::advancements_window_quad`]'s doc for the worked
    /// case), so every sub-rect sampled against one of these sheets is
    /// rescaled by `sprite.width/height` over this pair before it is added to
    /// the sprite's atlas offset — issue #582.
    const SHEET_DECLARED: (f32, f32) = (256.0, 256.0);
    /// [`Self::merchant`]'s own declared sheet size — see that field's doc
    /// comment for why it is genuinely `512×256` rather than `256×256` like
    /// every other sheet [`Self::SHEET_DECLARED`] covers.
    const MERCHANT_DECLARED: (f32, f32) = (512.0, 256.0);

    /// The Advancements screen's window blit (issue #167) —
    /// `graphics.blit(..., WINDOW_LOCATION, leftPos, topPos, 0, 0, 252, 140, 256,
    /// 256)` (`AdvancementsScreen.java`).
    ///
    /// The `252 x 140` sample is scaled by the sprite's **real placed size**
    /// against vanilla's declared `256 x 256` sheet
    /// (`BACKGROUND_TEXTURE_WIDTH`/`HEIGHT`, `AdvancementsScreen.java`) —
    /// issue #565's first defect ("the bottom and right side don't have UI on
    /// the edges"). `window.png` has no sibling `.mcmeta` (see this struct's
    /// own doc), so a higher-resolution pack is the only way this sheet's real
    /// size can differ from 256x256, and nothing else here would notice: the
    /// unscaled version always sampled a fixed 252x140 **real pixels**
    /// starting at the sprite's atlas origin, so a 2x pack (a `512x512`
    /// sheet) sampled only its top-left quarter — cropping the window's own
    /// bottom and right edges clean off, exactly the reported symptom. The
    /// same fraction-of-declared-size fix as the nine-slice arm (issue #561),
    /// applied to this hand-rolled sub-rect blit instead, since `window.png`
    /// is loose `textures/gui/container/**` art and never reaches
    /// [`lodestone_render::GuiAtlas`]'s `GuiScaling` system at all.
    #[must_use]
    pub(crate) fn advancements_window_quad(&self, x: f32, y: f32) -> Option<GuiSpriteQuad> {
        use crate::menu::advancements::{WINDOW_H, WINDOW_W};
        let sprite = self.atlas.sprite(&self.advancements_window)?;
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        let scale_x = sprite.width as f32 / Self::BACKGROUND_TEXTURE_SIZE;
        let scale_y = sprite.height as f32 / Self::BACKGROUND_TEXTURE_SIZE;
        let (sample_w, sample_h) = (WINDOW_W * scale_x, WINDOW_H * scale_y);
        Some(GuiSpriteQuad {
            dst: [x, y, WINDOW_W, WINDOW_H],
            uv_min: [sprite.x as f32 / aw, sprite.y as f32 / ah],
            uv_max: [
                (sprite.x as f32 + sample_w) / aw,
                (sprite.y as f32 + sample_h) / ah,
            ],
        })
    }

    /// One `16 x 16` tile of an advancement tab's background, by its datapack id.
    ///
    /// `full` is where the whole tile *would* go and `dst` is the part that
    /// survived the viewport clamp; the UV window is narrowed by the same
    /// fraction, which is what makes CPU clipping look like vanilla's scissor
    /// (`crate::menu::advancements`' module doc explains why there is no scissor).
    ///
    /// The fraction (`u0`/`v0`/`u1`/`v1`) is measured against `full`'s
    /// **declared** `16 x 16` size, since `full`/`dst` are already logical
    /// layout rects at that scale — but the fraction is then applied to the
    /// sprite's **real** placed size (`sprite.width`/`height`), not `full.w`/
    /// `full.h` again, for the same reason [`Self::advancements_window_quad`]
    /// scales its own sample: a higher-resolution tile texture has no
    /// `.mcmeta` to declare it, so `full.w`/`full.h` would otherwise be
    /// reused as a real-pixel span they are not.
    #[must_use]
    pub(crate) fn advancements_tile_quad(
        &self,
        id: &str,
        full: super::layout::Rect,
        dst: super::layout::Rect,
    ) -> Option<GuiSpriteQuad> {
        let loc = &self.advancements_tiles.iter().find(|(k, _)| *k == id)?.1;
        let sprite = self.atlas.sprite(loc)?;
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        let u0 = (dst.x - full.x) / full.w;
        let v0 = (dst.y - full.y) / full.h;
        let u1 = (dst.x + dst.w - full.x) / full.w;
        let v1 = (dst.y + dst.h - full.y) / full.h;
        let (sx, sy) = (sprite.x as f32, sprite.y as f32);
        let (real_w, real_h) = (sprite.width as f32, sprite.height as f32);
        Some(GuiSpriteQuad {
            dst: [dst.x, dst.y, dst.w, dst.h],
            uv_min: [(sx + u0 * real_w) / aw, (sy + v0 * real_h) / ah],
            uv_max: [(sx + u1 * real_w) / aw, (sy + v1 * real_h) / ah],
        })
    }

    /// [`sprite_quad`](Self::sprite_quad), reachable from outside this module —
    /// the Advancements screen (issue #167) draws every one of its widgets through
    /// it, and lives in `crate::menu` rather than here.
    #[must_use]
    pub(crate) fn sprite_quad_for(
        &self,
        id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) -> Option<GuiSpriteQuad> {
        self.sprite_quad(id, x, y, w, h)
    }

    /// The creative screen's own background blit (issue #158) —
    /// `graphics.blit(..., selectedTab.getBackgroundTexture(), leftPos, topPos,
    /// 0, 0, imageWidth, imageHeight, 256, 256)`
    /// (`CreativeModeInventoryScreen.java`), i.e. the top-left
    /// `195 x 136` window of a `256 x 256` sheet.
    ///
    /// A separate entry point from [`Self::quads`] rather than a
    /// [`BackgroundKind`] arm, because the creative screen has no
    /// [`Menu`] to dispatch on — see `super::creative`'s module doc.
    ///
    /// Rescales the `195x136` sample by the sprite's real placed size against
    /// [`Self::SHEET_DECLARED`], the same fix
    /// [`Self::advancements_window_quad`] applies to its own sheet — issue
    /// #582: these three sheets carry no `.mcmeta` either, so nothing short
    /// of this ratio notices a resource pack whose real pixels exceed the
    /// `256x256` baseline `CREATIVE_PANEL_W`/`CREATIVE_PANEL_H` are declared
    /// against.
    #[must_use]
    pub(super) fn creative_quad(
        &self,
        kind: super::creative::CreativeBackground,
        x: f32,
        y: f32,
    ) -> Option<GuiSpriteQuad> {
        use super::creative::{CREATIVE_PANEL_H, CREATIVE_PANEL_W, CreativeBackground};
        let loc = match kind {
            CreativeBackground::Items => &self.creative_items,
            CreativeBackground::ItemSearch => &self.creative_search,
            CreativeBackground::Inventory => &self.creative_inventory,
        };
        let sprite = self.atlas.sprite(loc)?;
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        let rx = sprite.width as f32 / Self::SHEET_DECLARED.0;
        let ry = sprite.height as f32 / Self::SHEET_DECLARED.1;
        let (sample_w, sample_h) = (CREATIVE_PANEL_W * rx, CREATIVE_PANEL_H * ry);
        Some(GuiSpriteQuad {
            dst: [x, y, CREATIVE_PANEL_W, CREATIVE_PANEL_H],
            uv_min: [sprite.x as f32 / aw, sprite.y as f32 / ah],
            uv_max: [
                (sprite.x as f32 + sample_w) / aw,
                (sprite.y as f32 + sample_h) / ah,
            ],
        })
    }

    /// The stitched atlas, for GPU upload via
    /// [`GpuAtlas::from_atlas`](lodestone_render::GpuAtlas::from_atlas).
    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The textured quad(s) vanilla's own `extractBackground` would blit for
    /// `menu`'s screen, with the panel's own top-left corner at `(x, y)` —
    /// see [`BackgroundKind`]'s doc comment for the Java call sites. `None`
    /// only if a sheet is missing from the atlas (never true of
    /// [`Self::build`]'s own output), which keeps this total rather than
    /// panicking on a hostile input.
    /// One whole GUI sprite (by the id [`GUI_SPRITES`] lists, or any
    /// `Slot::no_item_icon`) as a quad at `(x, y)` sized `w`×`h`.
    ///
    /// `None` when the sprite is not in the atlas, which [`Self::build`] makes
    /// impossible for its own output — the fallible signature exists so a caller
    /// drawing a `no_item_icon` this module does not know about degrades to
    /// drawing nothing rather than panicking.
    #[must_use]
    pub(super) fn sprite_quad(&self, id: &str, x: f32, y: f32, w: f32, h: f32) -> Option<GuiSpriteQuad> {
        let loc = sprite_location(id)?;
        let sprite = self.atlas.sprite(&loc)?;
        // `AtlasSprite`'s own normalised UVs cover the whole placed region,
        // which for these seven is the whole sprite — every one is static
        // (`frame_count == 1`), so there is no strip to index into.
        Some(GuiSpriteQuad {
            dst: [x, y, w, h],
            uv_min: sprite.uv_min,
            uv_max: sprite.uv_max,
        })
    }

    /// One status-effect icon (`mob_effect/<path>`, as
    /// `Hud.getMobEffectSprite` builds it) as a quad at `(x, y)` sized
    /// `w`x`h`.
    ///
    /// `None` when the pack has no icon for that effect, which is vanilla's
    /// missing-texture case; the caller draws no icon rather than a stand-in,
    /// because a stand-in is indistinguishable from art we failed to load.
    #[must_use]
    pub(super) fn mob_effect_icon_quad(
        &self,
        sprite_id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) -> Option<GuiSpriteQuad> {
        let loc = &self
            .mob_effect_icons
            .iter()
            .find(|(id, _)| id == sprite_id)?
            .1;
        let sprite = self.atlas.sprite(loc)?;
        Some(GuiSpriteQuad {
            dst: [x, y, w, h],
            uv_min: sprite.uv_min,
            uv_max: sprite.uv_max,
        })
    }

    /// A GUI sprite drawn at an arbitrary target size **through its declared
    /// `gui.scaling`** — vanilla's `blitSprite` with a width and height that
    /// are not the sprite's own.
    ///
    /// [`sprite_quad`](Self::sprite_quad) stretches the whole sprite over the
    /// target, which is right for a sprite blitted at its native size and
    /// wrong for a nine-slice one: the effect panel's background is a `32x32`
    /// sprite with a `4` px border drawn at up to a few hundred pixels wide,
    /// and stretching it smears the border across the whole widget.
    ///
    /// The decomposition itself is
    /// [`GuiScaling::geometry`](lodestone_assets::gui::GuiScaling::geometry)'s;
    /// this only turns each piece's source rect — in **native sprite pixels**,
    /// which is what `geometry` is handed and therefore returns — into an
    /// atlas UV window, and translates the destination rects by `(x, y)`.
    ///
    /// Returns an empty vector for an unstitched id, so a caller loops over
    /// nothing rather than drawing a wrong quad.
    #[must_use]
    pub(super) fn scaled_sprite_quads(
        &self,
        id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) -> Vec<GuiSpriteQuad> {
        let Some(loc) = sprite_location(id) else {
            return Vec::new();
        };
        let Some(sprite) = self.atlas.sprite(&loc) else {
            return Vec::new();
        };
        let scaling = self
            .sprite_scaling
            .iter()
            .find(|(k, _)| *k == id)
            .map_or(&GuiScaling::Stretch, |(_, v)| v);
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        let (sx, sy) = (sprite.x as f32, sprite.y as f32);
        scaling
            .geometry(
                sprite.width,
                sprite.height,
                w.max(0.0).round() as u32,
                h.max(0.0).round() as u32,
            )
            .into_iter()
            .map(|q| GuiSpriteQuad {
                dst: [
                    x + q.dst[0] as f32,
                    y + q.dst[1] as f32,
                    q.dst[2] as f32,
                    q.dst[3] as f32,
                ],
                uv_min: [(sx + q.src[0]) / aw, (sy + q.src[1]) / ah],
                uv_max: [
                    (sx + q.src[0] + q.src[2]) / aw,
                    (sy + q.src[1] + q.src[3]) / ah,
                ],
            })
            .collect()
    }

    /// A **sub-rectangle** of a static GUI sprite, sampled at `local`
    /// (`[lx, ly, lw, lh]`, in **declared** pixels against `declared`
    /// (`(width, height)`) — the size vanilla's own `blitSprite` call passes
    /// as `spriteWidth`/`spriteHeight`, not the sprite's real pixel size) and
    /// drawn at `dst` (`[x, y, w, h]`).
    ///
    /// [`sprite_quad`](Self::sprite_quad) always samples the *whole* sprite,
    /// which is right for the highlight pair and the empty-slot placeholders
    /// but wrong for the furnace family's lit/burn bars and the brewing
    /// stand's fuel/brew/bubble bars (issue #28): vanilla grows every one of
    /// those from a partial `blitSprite` sub-rectangle of a larger sprite —
    /// e.g. `AbstractFurnaceScreen.java`'s lit flame samples a `14×n`
    /// window of a `14×14` sprite, offset from the *bottom*, via
    /// `blitSprite(pipeline, sprite, 14, 14, 0, 14 - h, x, y, 14, h)`. That
    /// `14, 14` pair is `declared`, not a real pixel measurement, exactly
    /// like [`GuiScaling::geometry`](lodestone_assets::gui::GuiScaling::geometry)'s
    /// nine-slice case and
    /// [`GuiAtlas::subregion_quad_declared`](lodestone_render::GuiAtlas::subregion_quad_declared) —
    /// on a resource pack whose real pixels exceed `declared`, `local` is
    /// rescaled by `sprite.width/height` over `declared` before it is added
    /// to the sprite's atlas offset (issue #582: the un-rescaled version
    /// sampled only the top-left quadrant of a 32x sprite and drew it 2x too
    /// big). Mirrors the `uv` closure [`quads`](Self::quads) uses for the
    /// whole-panel sheets, generalised to any sprite in the atlas rather than
    /// the panel sheets specifically.
    #[must_use]
    pub(super) fn sprite_subregion_quad(
        &self,
        id: &str,
        declared: (f32, f32),
        local: [f32; 4],
        dst: [f32; 4],
    ) -> Option<GuiSpriteQuad> {
        let loc = sprite_location(id)?;
        let sprite = self.atlas.sprite(&loc)?;
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        let rx = sprite.width as f32 / declared.0;
        let ry = sprite.height as f32 / declared.1;
        let [lx, ly, lw, lh] = local;
        let (lx, ly, lw, lh) = (lx * rx, ly * ry, lw * rx, lh * ry);
        let uv_min = [
            (sprite.x as f32 + lx) / aw,
            (sprite.y as f32 + ly) / ah,
        ];
        let uv_max = [
            (sprite.x as f32 + lx + lw) / aw,
            (sprite.y as f32 + ly + lh) / ah,
        ];
        Some(GuiSpriteQuad {
            dst,
            uv_min,
            uv_max,
        })
    }

    #[must_use]
    pub(super) fn quads(&self, menu: &Menu, x: f32, y: f32) -> Option<Vec<GuiSpriteQuad>> {
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        // `declared` is the sheet's own declared (16x-baseline) size —
        // `Self::SHEET_DECLARED` for every sheet but the merchant's (see
        // `Self::MERCHANT_DECLARED`). `local` is rescaled by the sprite's
        // real placed size over `declared` before being added to the
        // sprite's atlas offset — issue #582; none of these sheets carries a
        // `.mcmeta`, so a resource pack has nowhere else to declare a higher
        // resolution and nothing upstream would otherwise notice one.
        let uv = |loc: &ResourceLocation,
                  declared: (f32, f32),
                  local: [f32; 4]|
         -> Option<([f32; 2], [f32; 2])> {
            let sprite = self.atlas.sprite(loc)?;
            let rx = sprite.width as f32 / declared.0;
            let ry = sprite.height as f32 / declared.1;
            let [lx, ly, lw, lh] = local;
            let (lx, ly, lw, lh) = (lx * rx, ly * ry, lw * rx, lh * ry);
            Some((
                [(sprite.x as f32 + lx) / aw, (sprite.y as f32 + ly) / ah],
                [
                    (sprite.x as f32 + lx + lw) / aw,
                    (sprite.y as f32 + ly + lh) / ah,
                ],
            ))
        };
        // A whole-panel `176x166` blit at the sheet's origin — the shape every
        // one of these single-image screens shares, except the hopper (see
        // `whole_panel_sized` below). Every sheet this closure is called with
        // is declared `Self::SHEET_DECLARED`.
        let whole_panel = |loc: &ResourceLocation| -> Option<Vec<GuiSpriteQuad>> {
            let (uv_min, uv_max) = uv(loc, Self::SHEET_DECLARED, [0.0, 0.0, 176.0, 166.0])?;
            Some(vec![GuiSpriteQuad {
                dst: [x, y, 176.0, 166.0],
                uv_min,
                uv_max,
            }])
        };
        // As `whole_panel`, but at an explicit size and declared sheet size —
        // the hopper's `176×133` (`HopperScreen.java`), the one screen in
        // this whole family that is not vanilla's usual `166` tall, and the
        // merchant's `276×166` off a genuinely `512×256`-declared sheet.
        let whole_panel_sized = |loc: &ResourceLocation,
                                  declared: (f32, f32),
                                  w: f32,
                                  h: f32|
         -> Option<Vec<GuiSpriteQuad>> {
            let (uv_min, uv_max) = uv(loc, declared, [0.0, 0.0, w, h])?;
            Some(vec![GuiSpriteQuad {
                dst: [x, y, w, h],
                uv_min,
                uv_max,
            }])
        };
        match background_kind(menu) {
            BackgroundKind::Inventory => whole_panel(&self.inventory),
            BackgroundKind::Crafting => whole_panel(&self.crafting),
            BackgroundKind::Anvil => whole_panel(&self.anvil),
            BackgroundKind::Grindstone => whole_panel(&self.grindstone),
            BackgroundKind::Smithing => whole_panel(&self.smithing),
            BackgroundKind::Enchantment => whole_panel(&self.enchantment),
            BackgroundKind::Furnace => whole_panel(&self.furnace),
            BackgroundKind::BlastFurnace => whole_panel(&self.blast_furnace),
            BackgroundKind::Smoker => whole_panel(&self.smoker),
            BackgroundKind::Brewing => whole_panel(&self.brewing_stand),
            BackgroundKind::Loom => whole_panel(&self.loom),
            BackgroundKind::Stonecutter => whole_panel(&self.stonecutter),
            BackgroundKind::Cartography => whole_panel(&self.cartography_table),
            BackgroundKind::Dispenser => whole_panel(&self.dispenser),
            BackgroundKind::Hopper => {
                whole_panel_sized(&self.hopper, Self::SHEET_DECLARED, 176.0, 133.0)
            }
            BackgroundKind::Merchant => {
                whole_panel_sized(&self.merchant, Self::MERCHANT_DECLARED, 276.0, 166.0)
            }
            BackgroundKind::Beacon => {
                whole_panel_sized(&self.beacon, Self::SHEET_DECLARED, 230.0, 219.0)
            }
            BackgroundKind::Generic { rows } => {
                let top_h = (rows * 18 + 17) as f32;
                let (top_min, top_max) =
                    uv(&self.generic, Self::SHEET_DECLARED, [0.0, 0.0, 176.0, top_h])?;
                let (bot_min, bot_max) =
                    uv(&self.generic, Self::SHEET_DECLARED, [0.0, 126.0, 176.0, 96.0])?;
                Some(vec![
                    GuiSpriteQuad {
                        dst: [x, y, 176.0, top_h],
                        uv_min: top_min,
                        uv_max: top_max,
                    },
                    GuiSpriteQuad {
                        dst: [x, y + top_h, 176.0, 96.0],
                        uv_min: bot_min,
                        uv_max: bot_max,
                    },
                ])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::{MemorySource, ResourceSource};

    fn solid_png(w: u32, h: u32) -> Vec<u8> {
        let mut data = Vec::new();
        let mut encoder = png::Encoder::new(&mut data, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        let pixels: Vec<u8> = (0..(w * h)).flat_map(|_| [10, 20, 30, 255]).collect();
        writer.write_image_data(&pixels).expect("png data");
        drop(writer);
        data
    }

    /// Declared (16x-baseline) real pixel size of a `gui/sprites/**` id this
    /// atlas stitches. Mirrors `container::tests::synthetic_background_with_window_size`'s
    /// own per-id table (kept independent rather than shared, since that
    /// helper lives in a large file this fix does not own) — every number
    /// here is the same vanilla-sourced literal, cross-checked against
    /// `crate::container::geometry`'s `*_DECLARED` constants for the five
    /// furnace/brewing ids.
    fn declared_sprite_size(id: &str) -> (u32, u32) {
        use super::super::{
            BLAST_FURNACE_BURN_PROGRESS, BLAST_FURNACE_LIT_PROGRESS, BREWING_BREW_PROGRESS,
            BREWING_BUBBLES, BREWING_FUEL_LENGTH, CELL, FURNACE_BURN_PROGRESS,
            FURNACE_LIT_PROGRESS, SLOT_HIGHLIGHT_BACK, SLOT_HIGHLIGHT_FRONT,
            SMOKER_BURN_PROGRESS, SMOKER_LIT_PROGRESS,
        };
        if id == SLOT_HIGHLIGHT_BACK || id == SLOT_HIGHLIGHT_FRONT {
            (24, 24)
        } else if id == FURNACE_LIT_PROGRESS
            || id == BLAST_FURNACE_LIT_PROGRESS
            || id == SMOKER_LIT_PROGRESS
        {
            (14, 14)
        } else if id == FURNACE_BURN_PROGRESS
            || id == BLAST_FURNACE_BURN_PROGRESS
            || id == SMOKER_BURN_PROGRESS
        {
            (24, 16)
        } else if id == BREWING_FUEL_LENGTH {
            (18, 4)
        } else if id == BREWING_BREW_PROGRESS {
            (9, 28)
        } else if id == BREWING_BUBBLES {
            (12, 29)
        } else if id.starts_with("container/creative_inventory/tab_") {
            (26, 32)
        } else if id.starts_with("container/creative_inventory/scroller") {
            (12, 15)
        } else if id.starts_with("advancements/tab_") {
            (28, 32)
        } else if id.ends_with("_frame_obtained") || id.ends_with("_frame_unobtained") {
            (26, 26)
        } else if id == "advancements/title_box" {
            (200, 26)
        } else {
            (CELL as u32, CELL as u32)
        }
    }

    /// A [`ContainerBackground`] fixture where every sheet and sprite is real
    /// at `scale`× its declared vanilla size. `scale == 1` is the
    /// 16x-equivalent input (declared == real) every other gate in this
    /// module used before issue #582 — the one input where the bug is
    /// invisible. `scale == 2` is a genuinely different ("32x") resolution:
    /// the discriminating input needed to tell a fraction-of-declared
    /// computation apart from a raw real-pixel one.
    fn synthetic_background_scaled(scale: u32) -> ContainerBackground {
        let mut src = MemorySource::default();
        for name in [
            "generic_54",
            "crafting_table",
            "inventory",
            "anvil",
            "grindstone",
            "smithing",
            "enchanting_table",
            "furnace",
            "blast_furnace",
            "smoker",
            "brewing_stand",
            "loom",
            "stonecutter",
            "cartography_table",
            "dispenser",
            "hopper",
            "beacon",
            // Real `villager.png` is `512x256` (`Self::MERCHANT_DECLARED`),
            // not `256x256`×scale like every sheet above — see that
            // constant's own doc.
            "creative_inventory/tab_items",
            "creative_inventory/tab_item_search",
            "creative_inventory/tab_inventory",
        ] {
            src.insert(
                format!("assets/minecraft/textures/gui/container/{name}.png"),
                solid_png(256 * scale, 256 * scale),
            );
        }
        src.insert(
            "assets/minecraft/textures/gui/container/villager.png".to_string(),
            solid_png(512 * scale, 256 * scale),
        );
        src.insert(
            "assets/minecraft/textures/gui/advancements/window.png".to_string(),
            solid_png(256 * scale, 256 * scale),
        );
        for id in ADVANCEMENT_TILE_IDS {
            let path = id.strip_prefix("minecraft:").unwrap_or(id);
            src.insert(
                format!("assets/minecraft/textures/{path}.png"),
                solid_png(16 * scale, 16 * scale),
            );
        }
        for id in super::super::all_gui_sprites() {
            let (dw, dh) = declared_sprite_size(id);
            src.insert(
                format!("assets/minecraft/textures/gui/sprites/{id}.png"),
                solid_png(dw * scale, dh * scale),
            );
        }
        let manager = ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>]);
        ContainerBackground::build(&manager).expect("synthetic background builds")
    }

    /// Fraction of `sprite`'s own placed atlas rect that quad `q` samples —
    /// resolution-independent by construction, so this is what the
    /// discriminating gates below compare across pack scales instead of raw
    /// UVs (whose absolute atlas placement can legitimately differ between
    /// two different-sized fixtures).
    fn uv_fraction(q: &GuiSpriteQuad, sprite_x: u32, sprite_y: u32, sprite_w: u32, sprite_h: u32, aw: f32, ah: f32) -> [f32; 4] {
        [
            (q.uv_min[0] * aw - sprite_x as f32) / sprite_w as f32,
            (q.uv_min[1] * ah - sprite_y as f32) / sprite_h as f32,
            (q.uv_max[0] * aw - sprite_x as f32) / sprite_w as f32,
            (q.uv_max[1] * ah - sprite_y as f32) / sprite_h as f32,
        ]
    }

    /// Issue #582, reproduced against every one of `sprite_subregion_quad`'s
    /// five real call sites (`crate::container::geometry`): the furnace
    /// family's lit/burn bars and the brewing stand's fuel/brew/bubble bars.
    /// Each is built at a 16x-equivalent (`scale = 1`) and a 32x
    /// (`scale = 2`) resolution, and the resolved UV span — as a fraction of
    /// the sprite's own real placed rect — must be identical at both: the
    /// same fraction `crate::container::geometry`'s `*_DECLARED` constants
    /// and vanilla's own `local` literals predict, computed here from
    /// outside the resolver rather than by calling it twice and comparing.
    ///
    /// A sub-rect genuinely smaller than the whole sprite is deliberate for
    /// every one of these (the lit flame's `[0, 14-h, 14, h]` for `h < 14`,
    /// the fuel bar's partial length, ...): a wrong ratio must actually move
    /// the sampled window, not merely fail to matter.
    #[test]
    fn sprite_subregion_quad_is_pack_resolution_independent() {
        use super::super::{
            BREWING_BREW_PROGRESS, BREWING_BUBBLES, BREWING_FUEL_LENGTH, FURNACE_BURN_PROGRESS,
            FURNACE_LIT_PROGRESS,
        };
        // (id, declared, local src) — the exact shapes `geometry.rs` requests
        // at less than full progress/length, so the window is a genuine
        // sub-rect of the sprite at every site.
        let cases: &[(&str, (f32, f32), [f32; 4])] = &[
            (FURNACE_LIT_PROGRESS, (14.0, 14.0), [0.0, 6.0, 14.0, 8.0]),
            (FURNACE_BURN_PROGRESS, (24.0, 16.0), [0.0, 0.0, 10.0, 16.0]),
            (BREWING_FUEL_LENGTH, (18.0, 4.0), [0.0, 0.0, 9.0, 4.0]),
            (BREWING_BREW_PROGRESS, (9.0, 28.0), [0.0, 0.0, 9.0, 14.0]),
            (BREWING_BUBBLES, (12.0, 29.0), [0.0, 12.0, 12.0, 17.0]),
        ];

        let mut mismatches: Vec<String> = Vec::new();
        for scale in [1u32, 2u32] {
            let bg = synthetic_background_scaled(scale);
            let (aw, ah) = (bg.atlas.width as f32, bg.atlas.height as f32);
            for (id, declared, local) in cases {
                let loc = sprite_location(id).expect("valid location");
                let sprite = bg.atlas.sprite(&loc).expect("sprite placed");
                let q = bg
                    .sprite_subregion_quad(id, *declared, *local, [0.0, 0.0, local[2], local[3]])
                    .unwrap_or_else(|| panic!("{id} at scale {scale} resolves"));
                let got = uv_fraction(&q, sprite.x, sprite.y, sprite.width, sprite.height, aw, ah);
                let want = [
                    local[0] / declared.0,
                    local[1] / declared.1,
                    (local[0] + local[2]) / declared.0,
                    (local[1] + local[3]) / declared.1,
                ];
                for i in 0..4 {
                    if (got[i] - want[i]).abs() >= 1e-4 {
                        mismatches.push(format!(
                            "{id} scale={scale} component {i}: got {:.6}, want {:.6}",
                            got[i], want[i]
                        ));
                    }
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "sprite_subregion_quad is not pack-resolution-independent:\n{}",
            mismatches.join("\n")
        );
    }

    /// As the gate above, but for [`ContainerBackground::quads`]'s
    /// whole-panel/`Generic` sub-rects — three representative
    /// [`BackgroundKind`] shapes exercising every branch of `quads`' `uv`
    /// closure: [`BackgroundKind::Hopper`] (`whole_panel_sized` against
    /// [`ContainerBackground::SHEET_DECLARED`]),
    /// [`BackgroundKind::Merchant`] (`whole_panel_sized` against
    /// [`ContainerBackground::MERCHANT_DECLARED`], the one sheet with a
    /// different declared size), and [`BackgroundKind::Generic`] (the
    /// two-piece top/bottom split).
    #[test]
    fn quads_whole_panel_is_pack_resolution_independent() {
        let cases: &[(&str, Menu, (f32, f32), [f32; 4])] = &[
            ("hopper", Menu::hopper(), (256.0, 256.0), [0.0, 0.0, 176.0, 133.0]),
            (
                "merchant",
                Menu::merchant(),
                (512.0, 256.0),
                [0.0, 0.0, 276.0, 166.0],
            ),
        ];

        let mut mismatches: Vec<String> = Vec::new();
        for scale in [1u32, 2u32] {
            let bg = synthetic_background_scaled(scale);
            let (aw, ah) = (bg.atlas.width as f32, bg.atlas.height as f32);
            for (label, menu, declared, local) in cases {
                let loc = match background_kind(menu) {
                    BackgroundKind::Hopper => &bg.hopper,
                    BackgroundKind::Merchant => &bg.merchant,
                    other => panic!("unexpected background_kind for {label}: {other:?}"),
                };
                let sprite = bg.atlas.sprite(loc).expect("sheet placed");
                let quads = bg.quads(menu, 0.0, 0.0).expect("quads resolve");
                let q = &quads[0];
                let got = uv_fraction(q, sprite.x, sprite.y, sprite.width, sprite.height, aw, ah);
                let want = [
                    local[0] / declared.0,
                    local[1] / declared.1,
                    (local[0] + local[2]) / declared.0,
                    (local[1] + local[3]) / declared.1,
                ];
                for i in 0..4 {
                    if (got[i] - want[i]).abs() >= 1e-4 {
                        mismatches.push(format!(
                            "{label} scale={scale} component {i}: got {:.6}, want {:.6}",
                            got[i], want[i]
                        ));
                    }
                }
            }

            // `Generic`'s two-piece split, at a row count that makes both the
            // top window and the bottom `126..222` window genuine sub-rects
            // (never the whole `256x256` sheet). `top_h` is
            // `background_kind`'s own documented formula
            // (`rows * 18 + 17`), not a guessed literal.
            let menu = Menu::generic(27);
            let rows = 27usize.div_ceil(9).clamp(1, 6);
            let top_h = (rows * 18 + 17) as f32;
            let loc = &bg.generic;
            let sprite = bg.atlas.sprite(loc).expect("generic sheet placed");
            let quads = bg.quads(&menu, 0.0, 0.0).expect("generic quads resolve");
            assert_eq!(quads.len(), 2, "generic chest is a two-piece blit");
            let expected = [
                (&quads[0], [0.0f32, 0.0, 176.0, top_h]),
                // bottom: [0, 126, 176, 96]
                (&quads[1], [0.0f32, 126.0, 176.0, 96.0]),
            ];
            for (q, local) in expected {
                let got = uv_fraction(q, sprite.x, sprite.y, sprite.width, sprite.height, aw, ah);
                let want = [
                    local[0] / 256.0,
                    local[1] / 256.0,
                    (local[0] + local[2]) / 256.0,
                    (local[1] + local[3]) / 256.0,
                ];
                for i in 0..4 {
                    if (got[i] - want[i]).abs() >= 1e-4 {
                        mismatches.push(format!(
                            "generic scale={scale} component {i}: got {:.6}, want {:.6}",
                            got[i], want[i]
                        ));
                    }
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "quads' whole-panel sub-rects are not pack-resolution-independent:\n{}",
            mismatches.join("\n")
        );
    }

    /// As the two gates above, for [`ContainerBackground::creative_quad`]'s
    /// `195x136`-declared sample.
    #[test]
    fn creative_quad_is_pack_resolution_independent() {
        use super::super::creative::{CREATIVE_PANEL_H, CREATIVE_PANEL_W, CreativeBackground};

        let mut mismatches: Vec<String> = Vec::new();
        for scale in [1u32, 2u32] {
            let bg = synthetic_background_scaled(scale);
            let (aw, ah) = (bg.atlas.width as f32, bg.atlas.height as f32);
            let sprite = bg.atlas.sprite(&bg.creative_items).expect("sheet placed");
            let q = bg
                .creative_quad(CreativeBackground::Items, 0.0, 0.0)
                .expect("creative quad resolves");
            let got = uv_fraction(&q, sprite.x, sprite.y, sprite.width, sprite.height, aw, ah);
            let want = [
                0.0,
                0.0,
                CREATIVE_PANEL_W / 256.0,
                CREATIVE_PANEL_H / 256.0,
            ];
            for i in 0..4 {
                if (got[i] - want[i]).abs() >= 1e-4 {
                    mismatches.push(format!(
                        "creative scale={scale} component {i}: got {:.6}, want {:.6}",
                        got[i], want[i]
                    ));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "creative_quad is not pack-resolution-independent:\n{}",
            mismatches.join("\n")
        );
    }
}
