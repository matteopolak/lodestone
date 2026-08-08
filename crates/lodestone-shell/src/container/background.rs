//! Vanilla's real `container/*.png` panel art, stitched into one small atlas.
//!
//! Split out of `container.rs` verbatim.

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
/// `ContainerScreen.java:21-27` draws the chest background as *two* blits (the
/// row-count-dependent top part, then a fixed 96 px bottom part immediately
/// below it), `CraftingScreen.java:29-34` and `InventoryScreen.java:96-101` each
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
/// (`AnvilScreen.java:30`-adjacent `blit` calls; every one of these screens'
/// `blit(texture, x, y, 0, 0, imageWidth, imageHeight)` uses the vanilla
/// `176×166` default, none override `imageWidth`/`imageHeight` — re-verified
/// against `AbstractContainerScreen.java:57-59`'s own default constructor for
/// the six added by #28, not merely assumed to match the first four). The one
/// exception is [`BackgroundKind::Hopper`]: `HopperScreen`'s constructor
/// explicitly passes `176, 133` (`HopperScreen.java:15`), so its blit is
/// `176×133`, not `166` — [`ContainerBackground::quads`] special-cases it
/// rather than reusing the `whole_panel` closure's hardcoded size.
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
            creative_items,
            creative_search,
            creative_inventory,
            advancements_window,
            advancements_tiles,
        })
    }

    /// The Advancements screen's window blit (issue #167) —
    /// `graphics.blit(..., WINDOW_LOCATION, leftPos, topPos, 0, 0, 252, 140, 256,
    /// 256)` (`AdvancementsScreen.java:205`).
    #[must_use]
    pub(crate) fn advancements_window_quad(&self, x: f32, y: f32) -> Option<GuiSpriteQuad> {
        use crate::menu::advancements::{WINDOW_H, WINDOW_W};
        let sprite = self.atlas.sprite(&self.advancements_window)?;
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        Some(GuiSpriteQuad {
            dst: [x, y, WINDOW_W, WINDOW_H],
            uv_min: [sprite.x as f32 / aw, sprite.y as f32 / ah],
            uv_max: [
                (sprite.x as f32 + WINDOW_W) / aw,
                (sprite.y as f32 + WINDOW_H) / ah,
            ],
        })
    }

    /// One `16 x 16` tile of an advancement tab's background, by its datapack id.
    ///
    /// `full` is where the whole tile *would* go and `dst` is the part that
    /// survived the viewport clamp; the UV window is narrowed by the same
    /// fraction, which is what makes CPU clipping look like vanilla's scissor
    /// (`crate::menu::advancements`' module doc explains why there is no scissor).
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
        Some(GuiSpriteQuad {
            dst: [dst.x, dst.y, dst.w, dst.h],
            uv_min: [(sx + u0 * full.w) / aw, (sy + v0 * full.h) / ah],
            uv_max: [(sx + u1 * full.w) / aw, (sy + v1 * full.h) / ah],
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
    /// (`CreativeModeInventoryScreen.java:742-744`), i.e. the top-left
    /// `195 x 136` window of a `256 x 256` sheet.
    ///
    /// A separate entry point from [`Self::quads`] rather than a
    /// [`BackgroundKind`] arm, because the creative screen has no
    /// [`Menu`] to dispatch on — see `super::creative`'s module doc.
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
        Some(GuiSpriteQuad {
            dst: [x, y, CREATIVE_PANEL_W, CREATIVE_PANEL_H],
            uv_min: [sprite.x as f32 / aw, sprite.y as f32 / ah],
            uv_max: [
                (sprite.x as f32 + CREATIVE_PANEL_W) / aw,
                (sprite.y as f32 + CREATIVE_PANEL_H) / ah,
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

    /// A **sub-rectangle** of a static GUI sprite, sampled at `local`
    /// (`[lx, ly, lw, lh]`, in the sprite's own native pixel space) and drawn
    /// at `dst` (`[x, y, w, h]`).
    ///
    /// [`sprite_quad`](Self::sprite_quad) always samples the *whole* sprite,
    /// which is right for the highlight pair and the empty-slot placeholders
    /// but wrong for the furnace family's lit/burn bars and the brewing
    /// stand's fuel/brew/bubble bars (issue #28): vanilla grows every one of
    /// those from a partial `blitSprite` sub-rectangle of a larger sprite —
    /// e.g. `AbstractFurnaceScreen.java:56-67`'s lit flame samples a `14×n`
    /// window of a `14×14` sprite, offset from the *bottom*. Mirrors the
    /// `uv` closure [`quads`](Self::quads) already uses for the three/eleven
    /// whole-panel sheets, generalised to any sprite in the atlas rather than
    /// the panel sheets specifically.
    #[must_use]
    pub(super) fn sprite_subregion_quad(
        &self,
        id: &str,
        local: [f32; 4],
        dst: [f32; 4],
    ) -> Option<GuiSpriteQuad> {
        let loc = sprite_location(id)?;
        let sprite = self.atlas.sprite(&loc)?;
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        let [lx, ly, lw, lh] = local;
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
        let uv = |loc: &ResourceLocation, local: [f32; 4]| -> Option<([f32; 2], [f32; 2])> {
            let sprite = self.atlas.sprite(loc)?;
            let [lx, ly, lw, lh] = local;
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
        // `whole_panel_sized` below).
        let whole_panel = |loc: &ResourceLocation| -> Option<Vec<GuiSpriteQuad>> {
            let (uv_min, uv_max) = uv(loc, [0.0, 0.0, 176.0, 166.0])?;
            Some(vec![GuiSpriteQuad {
                dst: [x, y, 176.0, 166.0],
                uv_min,
                uv_max,
            }])
        };
        // As `whole_panel`, but at an explicit size — the hopper's `176×133`
        // (`HopperScreen.java:15`), the one screen in this whole family that
        // is not vanilla's usual `166` tall.
        let whole_panel_sized = |loc: &ResourceLocation, w: f32, h: f32| -> Option<Vec<GuiSpriteQuad>> {
            let (uv_min, uv_max) = uv(loc, [0.0, 0.0, w, h])?;
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
            BackgroundKind::Hopper => whole_panel_sized(&self.hopper, 176.0, 133.0),
            BackgroundKind::Generic { rows } => {
                let top_h = (rows * 18 + 17) as f32;
                let (top_min, top_max) = uv(&self.generic, [0.0, 0.0, 176.0, top_h])?;
                let (bot_min, bot_max) = uv(&self.generic, [0.0, 126.0, 176.0, 96.0])?;
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
