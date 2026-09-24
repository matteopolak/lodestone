/// `kind` is the special-renderer id an item definition names
/// (`lodestone_assets::IconPart::Special::kind`); `item_path` is the item's own
/// registry path with the namespace stripped. Returns `(model name, texture stem)`
/// — the same two keys a placed block entity is batched on, because it is the same
/// rig and the same sheet.
///
/// # One resolver, every surface
///
/// This exists so a chest's rig is chosen in exactly **one** place for all five
/// surfaces vanilla draws these items on: the inventory slot, the first-person
/// hand, another entity's hand, a dropped stack, and an item frame. Two copies of
/// the mapping is how a chest ends up correct in the GUI and oak-coloured in the
/// hand — the copies differ in whichever `kind` was added later.
///
/// # Keyed by `kind` first and item path second, never by item path alone
///
/// The family is ten `kind`s over 91 item definitions. A `match` on the item id
/// alone would need 91 arms and would leave any datapack item invisible; the
/// `kind` says *what rig*, and the path only picks *which sheet* within it —
/// exactly vanilla's split, where vanilla's own unbaked chest special
/// renderer carries the `texture` field the item definition names.
///
/// `ChestHalf::Single` is not a simplification: vanilla's own unbaked chest
/// special renderer's `chest_type` defaults to SINGLE and no 26.2 item
/// definition overrides it, so an item chest is never one of the two double
/// halves.
///
/// # Which kinds resolve today
///
/// `chest` (13 item definitions), `shulker_box` (17), `head` (6),
/// `player_head` (1), `shield` (2, undyed/pattern-less only — see below),
/// `conduit` (1) and `copper_golem_statue` (8) — every `kind` whose rig is one
/// mesh and one sheet. The rest need more than a single pair and so have their
/// own entry points rather than an arm here:
///
/// | kind | items | why not a `(model, sheet)` pair |
/// |---|---|---|
/// | `banner` | 16 | the ordered translucent pattern-mask pass — [`banner_item_rig`] plus `crate::banner_pattern::banner_pattern_layers` |
/// | `decorated_pot` | 1 | five independently textured parts per instance — [`decorated_pot_item_rig`] |
/// | `trident` | 2 | its mesh is in the **entity** model set, not [`BLOCK_ENTITY_MODELS`] — [`trident_item_rig`] |
///
/// # The conduit item is one layer, not four
///
/// The obvious reading of vanilla's own conduit renderer is that a conduit
/// needs four (a model layer is baked for shell, cage, wind and eye), and an
/// earlier version of this doc said exactly that. That is the **block
/// entity**. Vanilla's own conduit-item special renderer bakes the shell
/// model layer alone and its own submit step issues one part-submit call
/// against its own inactive-shell texture — no cage, no wind, no eye,
/// because an item conduit is never *active*. So it is a plain pair, and it
/// always was.
///
/// # A copper golem statue item is always the **standing** pose
///
/// `copper_golem_statue.json` is a `select` on `minecraft:block_state`'s
/// `copper_golem_pose` property with a `standing` fallback, and an ordinary
/// stack carries no such property — so standing is what vanilla itself draws
/// for one in a hand or a slot. The eight item paths differ only in oxidation,
/// which is exactly what [`copper_golem_statue_oxidation_from_item_path`]
/// reads, so the two-level key lands the same way it does for a chest: the
/// `kind` picks the rig, the path picks the sheet. A stack that really does
/// carry a `minecraft:block_state` component naming another pose is the same
/// bounded shortfall `shield` records below — this signature has no room for
/// per-stack state.
///
/// **`shield` also resolves here now, but only ever as the undyed,
/// pattern-less rig.** The first-person hand and the GUI icon both bypass
/// this function and call [`shield_item_rig`] directly, because *they* carry
/// real per-stack state (`minecraft:base_color`, `minecraft:banner_patterns`)
/// this function's `(kind, item_path)` signature has no room for — see that
/// pair's own call sites (`lodestone_shell::gpu::first_person`'s
/// `prepare_special_hand`, `lodestone_shell::hud::item_icon`'s GUI-icon
/// pass). But this resolver's three callers (a dropped stack, another
/// entity's hand, an item frame) had **no** shield arm at all until this one
/// landed — `_ => None` swallowed every one of them, so a dropped or framed
/// shield drew nothing, full stop, not merely undyed. Resolving it here to
/// the no-pattern sheet unconditionally is the same *bounded* shortfall this
/// module already accepts elsewhere on these three surfaces (a dropped
/// stack's own doc: "no stack multiplication"; a framed item's own doc: "the
/// in-frame rotation is undecoded") — a real shield reaching real pixels,
/// just not its dye or loom pattern, because neither surface threads that
/// state through `EntityDraw` today.
///
/// `None` is also the right answer for an item path a `kind` does not recognise (a
/// datapack item declaring `minecraft:chest` over something that is not a chest):
/// drawing nothing beats drawing a plain oak chest for it. **All seven head paths
/// resolve**, including `dragon_head`/`piglin_head`, which reach their own
/// multi-part rigs rather than a skull layer; vanilla scales the dragon head's
/// icon down through its base model's own `gui` display transform
/// (`item/dragon_head.json`, `scale 0.6`), which is the caller's own display
/// transform and not this function's business.
#[must_use]
pub fn special_item_rig(kind: &str, item_path: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "minecraft:chest" => {
            let material = ChestMaterial::from_block_path(item_path)?;
            Some((
                CHEST_SINGLE,
                chest_texture_stem(material, ChestHalf::Single),
            ))
        }
        "minecraft:shulker_box" => {
            // `shulker_box` (the undyed one) has no colour prefix and takes the
            // default sheet; every other path is `<colour>_shulker_box`. Passing
            // the whole path through would silently take the default arm for all
            // seventeen, which is the plausible wrong version: it draws, and it
            // draws purple.
            let colour = item_path.strip_suffix("_shulker_box").filter(|c| {
                // Only a real dye colour — a datapack `foo_shulker_box` should
                // not quietly become the default sheet.
                SHULKER_COLOURS.contains(c)
            });
            if colour.is_none() && item_path != "shulker_box" {
                return None;
            }
            Some((SHULKER_BOX, shulker_texture_stem(colour)))
        }
        // Two `kind`s, one rig family: vanilla splits `player_head` out because
        // its renderer resolves a profile texture. This function has no stack in
        // hand, so it answers for a *plain* head and returns the default Steve
        // stem; a custom head's own sheet is substituted by the caller, which
        // does — the shell's GUI icon pass and its placed-head pass both replace
        // this stem with a `BlockEntityTexture::PlayerSkin`.
        "minecraft:head" | "minecraft:player_head" => {
            let ty = SkullType::from_block_path(item_path)?;
            Some((ty.model(), skull_texture_stem(ty)))
        }
        // Always the no-pattern sheet — see this function's own doc for why
        // a dyed or patterned shield still only reaches pixels through the
        // hand/GUI call sites that bypass this resolver entirely.
        "minecraft:shield" => Some((SHIELD, SHIELD_BASE_NO_PATTERN_TEXTURE_STEM)),
        // One layer, not four — vanilla's own conduit-item special renderer
        // takes the shell model alone. See this function's own doc for why the
        // four-layer reading is about the block entity instead.
        "minecraft:conduit" if item_path == "conduit" => {
            Some((CONDUIT_SHELL, CONDUIT_SHELL_TEXTURE_STEM))
        }
        // Always the standing pose: an item stack has no `copper_golem_pose`
        // block-state property, so vanilla's own `select` takes its fallback.
        "minecraft:copper_golem_statue" => {
            let oxidation = copper_golem_statue_oxidation_from_item_path(item_path)?;
            Some((
                CopperGolemPose::Standing.model_name(),
                copper_golem_statue_texture_stem(oxidation),
            ))
        }
        _ => None,
    }
}

/// A copper golem statue **item**'s oxidation level, from its own registry
/// path — the item-side twin of the block-state-keyed resolver the shell's
/// placed-statue pass uses, and the reason [`special_item_rig`] can pick a
/// sheet for all eight paths without a block state.
///
/// `waxed_` is stripped first: waxing halts further weathering but does not
/// change which of the four sheets a statue draws, and vanilla's own
/// oxidation-level table has no fifth, waxed-specific entry. That makes
/// the eight item paths four sheets, which is exactly what the eight item
/// definitions in the 26.2 jar name — `waxed_oxidized_copper_golem_statue.json`
/// and `oxidized_copper_golem_statue.json` both carry
/// `entity/copper_golem/copper_golem_oxidized`.
///
/// `None` for any other path, so a datapack item declaring
/// `minecraft:copper_golem_statue` over something that is not one draws nothing
/// rather than an unaffected-copper statue.
#[must_use]
pub fn copper_golem_statue_oxidation_from_item_path(
    item_path: &str,
) -> Option<CopperGolemOxidation> {
    let path = item_path.strip_prefix("waxed_").unwrap_or(item_path);
    Some(match path {
        "copper_golem_statue" => CopperGolemOxidation::Unaffected,
        "exposed_copper_golem_statue" => CopperGolemOxidation::Exposed,
        "weathered_copper_golem_statue" => CopperGolemOxidation::Weathered,
        "oxidized_copper_golem_statue" => CopperGolemOxidation::Oxidized,
        _ => return None,
    })
}

/// [`decorated_pot_item_rig`]'s result: five opaque draws sharing one
/// placement, in vanilla's own decorated-pot renderer's own submission order.
///
/// Five rather than two (a banner) or one (a shield) because the thing that
/// varies here is the **diffuse sheet per quad**, not a tint over one mesh —
/// which is a shape the ordinary batcher was already built for. See
/// [`BlockEntityModelSet::resolve_decorated_pot`], the placed-pot twin, whose
/// own doc carries the derivation; this is the same decomposition at the item
/// surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoratedPotItemRig {
    /// `(model, texture)` for the neck/top/bottom body — always the base sheet.
    pub base: (&'static str, &'static str),
    /// `(model, texture)` for the front face.
    pub front: (&'static str, &'static str),
    /// `(model, texture)` for the back face.
    pub back: (&'static str, &'static str),
    /// `(model, texture)` for the left face.
    pub left: (&'static str, &'static str),
    /// `(model, texture)` for the right face.
    pub right: (&'static str, &'static str),
}

impl DecoratedPotItemRig {
    /// The five draws in submission order, for a caller that wants to iterate
    /// rather than name each face — the shape every consumer actually uses.
    #[must_use]
    pub const fn parts(&self) -> [(&'static str, &'static str); 5] {
        [self.base, self.front, self.back, self.left, self.right]
    }
}

/// The `minecraft:decorated_pot` item rig — vanilla's own decorated-pot-item
/// special renderer's submit step, which forwards straight to the same
/// decorated-pot renderer submit step a placed pot uses, substituting an
/// empty decorations value when the stack carries none.
///
/// The four arguments are the sherd **item paths** off the stack's own
/// `minecraft:pot_decorations` component, namespace stripped, in vanilla's
/// record order. `None` is a plain brick face rather than "unknown" —
/// Vanilla's own pot-decorations accessor maps the brick item to "absent" on
/// the way in, so a blank face and a brick face are the same state by
/// construction.
///
/// # Every side always draws
///
/// An undecorated side takes [`DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM`] rather
/// than being skipped, because vanilla's own submit step submits a model
/// part for all four faces unconditionally. Skipping blank sides would draw a pot with
/// three invisible faces for the overwhelmingly common undecorated stack, and
/// then **silently autocorrect** the moment a player added their first sherd —
/// which is the failure that looks like a component-decode bug rather than a
/// draw one.
///
/// # This returns the whole rig even when a sherd is unrecognised
///
/// A datapack sherd path that [`decorated_pot_pattern_texture_stem`] declines
/// falls back to the default side sprite for **that face only**. The pot still
/// draws. That is deliberately unlike [`special_item_rig`]'s "decline the whole
/// item rather than guess", and the asymmetry is the point: there, an
/// unrecognised path means we do not know what rig the item wants; here we know
/// exactly what rig it wants and only one of its four sheets is unknown, and
/// vanilla's own side-sprite lookup takes precisely this fallback.
///
/// Unlike a banner's, no argument here is optional-by-shortfall — the sherds are
/// a real decoded component (`lodestone_model::PotDecorations`), so a caller
/// that has the stack can pass the truth.
#[must_use]
pub fn decorated_pot_item_rig(
    back: Option<&str>,
    left: Option<&str>,
    right: Option<&str>,
    front: Option<&str>,
) -> DecoratedPotItemRig {
    let side = |sherd: Option<&str>| -> &'static str {
        sherd
            .and_then(decorated_pot_pattern_texture_stem)
            .unwrap_or(DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM)
    };
    DecoratedPotItemRig {
        base: (DECORATED_POT_BASE, DECORATED_POT_BASE_TEXTURE_STEM),
        front: (DECORATED_POT_SIDE_FRONT, side(front)),
        back: (DECORATED_POT_SIDE_BACK, side(back)),
        left: (DECORATED_POT_SIDE_LEFT, side(left)),
        right: (DECORATED_POT_SIDE_RIGHT, side(right)),
    }
}

/// The entity-corpus model name a held `minecraft:trident` draws —
/// vanilla's own trident-item special renderer bakes its trident model from
/// the trident model layer.
///
/// # Why this is not an arm in [`special_item_rig`]
///
/// Not because the rig is unported — it has been in the tree all along. The
/// trident's mesh is `lodestone_assets::entity_models`' `"trident"` entry, the
/// same one vanilla's own thrown-trident renderer draws a thrown trident with, and
/// [`special_item_rig`]'s contract is that its `&'static str` is a key into
/// [`BLOCK_ENTITY_MODELS`]. Returning an entity-corpus name from there would
/// type-check, resolve to nothing in every one of that function's callers, and
/// draw exactly the blank hand this exists to fix — so the *corpus* is part of
/// the return type's meaning, and a separate entry point is how that is said.
///
/// This mirrors vanilla more closely than folding it in would: every one of
/// these renderers bakes out of `context.entityModelSet()`, and which corpus a
/// rig happens to live in on our side is our own storage decision, not a
/// statement about the item.
///
/// # The sheet is the corpus entry's own
///
/// Unlike a chest, there is no second key to return: the entity corpus binds a
/// texture per entry (`EntityTexture::Fixed("entity/trident/trident")`, which is
/// vanilla's own trident-model texture), so a caller looks the sheet up by
/// this same name
/// rather than by a stem. That is why this returns one string and not a pair.
///
/// # The GUI is deliberately not a caller
///
/// `trident.json` is a `select` on `minecraft:display_context` whose
/// `gui`/`ground`/`fixed`/`on_shelf` case is a plain `minecraft:model`
/// (`item/trident`, the flat sprite) — only the *fallback* reaches a
/// `minecraft:trident` special node. So an inventory trident is a sprite in
/// vanilla too, and a rig in the slot would be the regression, not the fix.
///
/// `None` for any path that is not the trident itself, so a datapack item
/// naming this `kind` over something else draws nothing rather than a trident.
#[must_use]
pub fn trident_item_rig(item_path: &str) -> Option<&'static str> {
    (item_path == "trident").then_some(TRIDENT_ENTITY_MODEL)
}

/// The `lodestone_assets::entity_models` corpus entry a trident is registered
/// under — shared by [`trident_item_rig`] and vanilla's own thrown-trident
/// renderer's own projectile path so the held and thrown tridents cannot
/// drift onto two
/// meshes.
pub const TRIDENT_ENTITY_MODEL: &str = "trident";

/// The `minecraft:banner` item rig — [`special_item_rig`]'s own doc table lists
/// this `kind` as one of the six that resolve to `None` ("needs the ordered
/// translucent pattern-mask pass, not one rig"), which was the honest state the
/// day that table was written and is also the reason a banner drew nothing at
/// all in a hotbar slot, an inventory slot or the first-person hand — not a
/// missing draw call, a `kind` this dispatcher never recognised.
///
/// # Two landings, and this is the second
///
/// The first landing multiplied the base colour directly into the opaque flag
/// texture — an approximation, disclosed at the time: vanilla's own opaque
/// body+flag pass (its own plain wood/cloth sheet
/// [`BANNER_BASE_TEXTURE_STEM`] names) is drawn **untinted**, and everything
/// the player perceives as colour is a *second*, translucent
/// pattern-submit draw layered over it — first the base mask tinted by the
/// base colour, then up to 16 loom pattern masks, each its own draw
/// (vanilla's own banner-pattern submit step). A single flat tint cannot show a
/// pattern at all.
///
/// Now that `minecraft:banner_patterns` decodes to a real, typed value (see
/// [`lodestone_model::ItemComponents::banner_patterns`]) and a caller can
/// derive the same ordered mask list a placed banner uses
/// (`crate::banner_pattern::banner_pattern_layers`, fed by
/// [`banner_item_base_color`] plus the decoded patterns), this rig's own two
/// meshes draw **untinted**, exactly like
/// [`BlockEntityModelSet::resolve_banner`]'s own `body`/`flag` — matching
/// vanilla's opaque pass exactly, with every bit of colour riding the
/// caller's own translucent layer draws instead. Reusing the field name
/// `flag_tint` for "no tint" would have been the same defaulting trap
/// `DESIGN.md` §12 records for a trait method with a `true` default: the
/// safer shape is a rig with no colour field to forget to multiply in twice.
///
/// Returns `None` for an item path that is not `<dye>_banner` — a shield, or a
/// datapack item naming this `kind` over something else.
#[must_use]
pub fn banner_item_rig(item_path: &str) -> Option<BannerItemRig> {
    banner_item_base_color(item_path)?;
    Some(BannerItemRig {
        body: (BANNER_BODY, BANNER_BASE_TEXTURE_STEM),
        flag: (BANNER_FLAG, BANNER_BASE_TEXTURE_STEM),
    })
}

/// The base dye colour parsed from a banner item's own path (`<dye>_banner`,
/// via [`crate::banner_pattern::DyeColor::from_name`]) — factored out of
/// [`banner_item_rig`] so a caller building the *translucent* pattern-layer
/// draws (base mask plus every loom pattern, via
/// [`crate::banner_pattern::banner_pattern_layers`]) derives the same base
/// colour [`banner_item_rig`] validated, rather than re-parsing the item path
/// a second, potentially diverging way. `None` for the same inputs
/// [`banner_item_rig`] rejects.
#[must_use]
pub fn banner_item_base_color(item_path: &str) -> Option<crate::banner_pattern::DyeColor> {
    let name = item_path.strip_suffix("_banner")?;
    crate::banner_pattern::DyeColor::from_name(name)
}

/// [`banner_item_rig`]'s result: two opaque draws sharing one placement, both
/// **untinted** — see that function's doc for why colour no longer lives
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerItemRig {
    /// `(model, texture)` for the pole/bar.
    pub body: (&'static str, &'static str),
    /// `(model, texture)` for the flag — the same mesh a caller's translucent
    /// pattern-layer draws paint over.
    pub flag: (&'static str, &'static str),
}

// A skull special renderer submits the raw Y-down skull model cube
// (built from an `addBox(-4, -8, -4, 8, 8, 8)`-shaped box, matching this crate's
// `SKULL_HUMANOID`/`SKULL_MOB` AABB). It does not supply a pose itself: 26.2's
// `items/player_head.json` and every ordinary `items/*_head.json` instead put
// `T(0.5, 0, 0.5) * Rx(180°)` on the `minecraft:special` model node. The parser
// retains that whole root-to-node chain and every consumer folds it through
// `compose_special_item_transform`. If a server pack retargets player heads to
// a bare `minecraft:head` special with an empty chain, that shared compositor
// restores this exact canonical wrapper once; parsed chains remain untouched.
use super::super::model_families::*;
