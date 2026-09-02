//! The `Display` entity family's draw-ready snapshot: `text_display`/
//! `item_display`/`block_display`, extracted from the ingest ECS into the
//! plain [`DisplayDraw`] PODs `gpu/display_text.rs` consumes.
//!
//! All three subtypes have a GPU consumer. `text_display` draws through
//! `gpu/display_text.rs`; `block_display` through
//! `RenderState::merge_block_displays` in `gpu/moving_blocks.rs` (the same
//! "block model posed by hand" seam a falling block and a piston head use);
//! `item_display` through `RenderState::merge_item_displays` in
//! `gpu/world_items.rs` (the same item-model seam a framed or dropped item
//! uses). Both were a *producer* for an existing consumer, not new rendering.
//!
//! [`DisplayDraw::placement`] is the composition all three share, extracted as
//! a named symbol so the billboard orientation and the synced
//! `Transformation` cannot be applied two different ways in three files.
//!
//! # Why this is a separate extract system from [`crate::entities::extract_entity_draws`]
//!
//! That system reads through a **render-side interpolation track**
//! (`spawn_track`/`update_track`, `EntityDraw`'s `feet`/`yaw`/`pitch` are
//! smoothed between server ticks) and is already at bevy's `SystemParam`
//! tuple-arity ceiling — its own comments say so at three separate call
//! sites. A `Display` entity's billboard/transformation fields change only
//! when a `/data merge` or a plugin edits them, essentially never every
//! tick, so there is nothing to smooth: this system reads the **ingest**
//! entity's components directly (the same entity [`lodestone_ecs::ingest`]'s
//! systems write to), with no render-side track, no `EntityIndex` bridge and
//! no interpolation clock. That is a deliberate, disclosed simplification —
//! see [`DisplayDraw::position`]'s own doc — not an oversight; a
//! `block_display` snapping to a new position on the tick it is announced
//! reads as "instant" rather than "smooth", which is a fidelity loss, not a
//! correctness one.
//!
//! # The chain this closes
//!
//! `lodestone_render::display` (`BillboardMode`, `DisplayTransformation`,
//! `display_orientation`/`display_placement_matrix`) has been real, tested
//! geometry with a disclosed *absence of a producer* since it landed —
//! `crates/protocol/v770` had no metadata decode for the `Display` family at
//! all. That protocol-layer gap is closed in
//! `crates/protocol/v770/src/packets/metadata.rs`, and
//! `lodestone_ecs::ingest::apply_display_metadata` folds the result into the
//! `Display*` components this file queries. This is the next hop: ingest
//! components → draw-ready snapshot. The **last** hop — a GPU pass that
//! actually rasterises one — is `gpu/display_text.rs` for `text_display`.
//! That pass installs from `app::redraw` via `Sim::display_draws()` ->
//! `RenderState::set_display_draws` every frame — `RenderState` had this
//! setter with no production caller for a while after it landed, which is
//! its own note on that method.

use bevy_ecs::prelude::{IntoScheduleConfigs, Query, ResMut, Resource};
use glam::{Quat, Vec3};
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::entity::{
    DisplayBackgroundColor, DisplayBillboard, DisplayBlockState, DisplayBrightness, DisplayItem,
    DisplayItemContext, DisplayLeftRotation, DisplayLineWidth, DisplayRightRotation, DisplayScale,
    DisplayStyleFlags, DisplayText, DisplayTextOpacity, DisplayTranslation, EntityKind,
    MinecraftEntityId, Position, Rotation,
};
use lodestone_ecs::{Extract, ExtractSet};
use lodestone_model::ItemStack;

/// Vanilla's own text-display entity's registry path, as [`DisplayDraw::type_path`]
/// carries it (bare path, no namespace — matching
/// `lodestone_shell::entities::EntityDraw::type_path`'s own convention).
pub const TEXT_DISPLAY_TYPE_PATH: &str = "text_display";
/// Vanilla's own item-display entity's registry path.
pub const ITEM_DISPLAY_TYPE_PATH: &str = "item_display";
/// Vanilla's own block-display entity's registry path.
pub const BLOCK_DISPLAY_TYPE_PATH: &str = "block_display";

/// Vanilla's own text-display entity accessor defaults, applied wherever the
/// corresponding component has never been reported — matching every default
/// this repo already documents on the `Display*` components themselves
/// (`lodestone_ecs::entity::DisplayLineWidth` etc.).
const DEFAULT_LINE_WIDTH: i32 = 200;
const DEFAULT_BACKGROUND_COLOR: i32 = 0x4000_0000_u32 as i32;
const DEFAULT_TEXT_OPACITY: i8 = -1;
/// Vanilla's own item-display entity's own accessor default for `DATA_ITEM_DISPLAY_ID`:
/// its own "no context" enum value, ordinal `0`.
///
/// **An earlier version of this constant was `FIXED`, on the stated grounds
/// that `NONE` "draws nothing at all". That is false.** Vanilla's own
/// item-transforms get-transform routine
/// answers every context it has no `display` key for — `NONE` included — with
/// its own no-op transform, i.e. the identity pose, so a context-less
/// `item_display` draws its model *unscaled and unrotated*, which is exactly
/// what `/summon item_display {item:{…}}` looks like in vanilla. Defaulting to
/// `FIXED` instead would silently apply the item frame's own half-scale pose to
/// every hologram that never reported a context. `lodestone_assets::DisplaySlot`
/// records the same fact from the other side: it deliberately has no `NONE`
/// variant, because that context selects no `display` key.
const DEFAULT_ITEM_DISPLAY_CONTEXT: u8 = ItemDisplayContextOrdinal::NONE;

/// The `ItemDisplayContext` ordinals this seam's consumers name rather than
/// leave as bare integers — transcribed from vanilla's own item-display-context
/// declaration, which assigns each ordinal explicitly (`NONE(0)`,
/// `THIRD_PERSON_LEFT_HAND(1)`, `THIRD_PERSON_RIGHT_HAND(2)`,
/// `FIRST_PERSON_LEFT_HAND(3)`, `FIRST_PERSON_RIGHT_HAND(4)`, `HEAD(5)`,
/// `GUI(6)`, `GROUND(7)`, `FIXED(8)`, `ON_SHELF(9)`) rather than leaning on
/// declaration order.
struct ItemDisplayContextOrdinal;
impl ItemDisplayContextOrdinal {
    const NONE: u8 = 0;
}

/// The [`lodestone_assets::DisplaySlot`] an `ItemDisplayContext` ordinal
/// selects, or `None` for `NONE` (and for any out-of-range byte, which
/// vanilla's own by-id lookup's out-of-bounds "zero" strategy also folds
/// onto `NONE`).
///
/// `None` is a real answer, not a failure: vanilla's own get-transform routine returns
/// its own no-op transform for it, so the caller poses the model with
/// `DisplayTransform::default()` — see [`DEFAULT_ITEM_DISPLAY_CONTEXT`].
#[must_use]
pub fn display_slot_for_context(ordinal: u8) -> Option<lodestone_assets::DisplaySlot> {
    use lodestone_assets::DisplaySlot;
    match ordinal {
        1 => Some(DisplaySlot::ThirdPersonLeftHand),
        2 => Some(DisplaySlot::ThirdPersonRightHand),
        3 => Some(DisplaySlot::FirstPersonLeftHand),
        4 => Some(DisplaySlot::FirstPersonRightHand),
        5 => Some(DisplaySlot::Head),
        6 => Some(DisplaySlot::Gui),
        7 => Some(DisplaySlot::Ground),
        8 => Some(DisplaySlot::Fixed),
        9 => Some(DisplaySlot::OnShelf),
        _ => None,
    }
}

/// One `Display`-family entity's draw-ready snapshot for this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayDraw {
    /// The server-assigned entity id.
    pub id: i32,
    /// One of [`TEXT_DISPLAY_TYPE_PATH`]/[`ITEM_DISPLAY_TYPE_PATH`]/
    /// [`BLOCK_DISPLAY_TYPE_PATH`] — the switch every consumer of this type
    /// keys on.
    pub type_path: &'static str,
    /// The entity's last-reported world position (its own feet/anchor, the
    /// same value every other entity type's position means).
    ///
    /// **Not interpolated** between server ticks, unlike
    /// `lodestone_shell::entities::EntityDraw::feet` — see this module's own
    /// doc for why that is a deliberate, disclosed simplification for this
    /// entity family rather than an oversight.
    pub position: Vec3,
    /// The entity's own reported yaw, degrees — vanilla's own fixed-billboard
    /// yaw source (its own entity yaw getter). Irrelevant to every other billboard
    /// mode, which reads the *camera's* yaw instead — see
    /// `lodestone_render::display::display_orientation`.
    pub entity_yaw: f32,
    /// The entity's own reported pitch, degrees — `Fixed`'s pitch source.
    pub entity_pitch: f32,
    /// Vanilla's own billboard-constraints enum, resolved from the raw wire ordinal —
    /// `Fixed` (vanilla's own accessor default) when never reported.
    pub billboard: lodestone_render::display::BillboardMode,
    /// The shared `translation`/`left_rotation`/`scale`/`right_rotation`
    /// transformation, with every unreported field at vanilla's own
    /// accessor default (see [`lodestone_render::display::DisplayTransformation::default`]).
    pub transform: lodestone_render::display::DisplayTransformation,
    /// `text_display`'s current text — `None` when either this is not a
    /// `text_display` or it is one that has never reported the field
    /// (vanilla's own default is the empty string, which draws nothing
    /// either way, so the two collapse to the same "draw nothing" case).
    ///
    /// The full styled component tree, carried straight through from
    /// [`lodestone_ecs::entity::DisplayText`] — `gpu/display_text.rs` calls
    /// [`lodestone_model::Text::to_spans`] on it directly, so colour (hex
    /// included), bold, italic, underline and strikethrough all survive, with
    /// no `to_legacy_string`/`from_legacy` round trip to lose a hex colour
    /// along the way (legacy `§` codes have no hex form).
    pub text: Option<lodestone_model::Text>,
    /// `text_display`'s wrap width in pixels, defaulted to vanilla's own
    /// `200` when unreported. Meaningless for the other two subtypes.
    pub text_line_width: i32,
    /// `text_display`'s background panel colour, packed ARGB, defaulted to
    /// vanilla's own `0x4000_0000` (translucent black) when unreported.
    /// `0` means "no panel at all" (vanilla's own `if (backgroundColor != 0)`
    /// gate) — a real, reachable value, not a sentinel.
    pub text_background_color: i32,
    /// `text_display`'s text alpha, defaulted to vanilla's own `-1` (fully
    /// opaque) when unreported.
    pub text_opacity: i8,
    /// `text_display`'s style-flags byte, defaulted to `0` when unreported
    /// (no shadow, opaque, explicit background colour, centre-aligned).
    pub text_style_flags: u8,
    /// `block_display`'s imitated block state (a global block-state id) —
    /// `None` when either this is not a `block_display` or it is one that
    /// has never reported the field, which is the switch a block-display
    /// consumer keys on, exactly as
    /// `lodestone_shell::entities::EntityDraw::block_state` already does for
    /// a falling block.
    pub block_state: Option<u32>,
    /// `item_display`'s displayed item stack — `None` when either this is
    /// not an `item_display` or it is one whose stack has never been
    /// reported (an empty `ItemStack::default()`-shaped absence, same
    /// contract as `lodestone_ecs::entity::DisplayItem`).
    pub item: Option<ItemStack>,
    /// `item_display`'s `ItemDisplayContext` ordinal, defaulted to vanilla's
    /// own `FIXED` (the item-frame-style no-perspective context) when
    /// unreported — see [`DEFAULT_ITEM_DISPLAY_CONTEXT`] for why that default
    /// is `NONE` rather than `FIXED`.
    pub item_display_context: u8,
    /// Vanilla's own brightness-override data accessor, vanilla's **packed** form
    /// (its own pack routine — `block << 4 | sky << 20`), or `None` when the
    /// entity reported no override. Shared by all three subtypes.
    ///
    /// `-1` is vanilla's own "no override" sentinel and is folded to `None` at
    /// the extract, so a consumer never has to know the sentinel; use
    /// [`Self::override_light`] rather than unpacking this by hand.
    pub brightness_override: Option<i32>,
}

impl DisplayDraw {
    /// The full local-space-to-world matrix every subtype's content is drawn
    /// through: `T(position) · orientation · transformation`.
    ///
    /// Extracted as a named symbol because three consumers in three files
    /// compose it, and this repo has already paid for a defect that lived in
    /// the *composition* of two individually-correct halves. It is
    /// [`lodestone_render::display::display_placement_matrix`] with the
    /// billboard resolved from this draw's own mode — the two cannot be
    /// applied out of step because there is one expression.
    ///
    /// `text_display` does **not** call this today: `gpu/display_text.rs`
    /// reaches the same two functions itself, and folding it in is a change to
    /// a file this seam does not own.
    #[must_use]
    pub fn placement(&self, camera_yaw_deg: f32, camera_pitch_deg: f32) -> glam::Mat4 {
        lodestone_render::display::display_placement_matrix(
            self.position,
            lodestone_render::display::display_orientation(
                self.billboard,
                self.entity_yaw,
                self.entity_pitch,
                camera_yaw_deg,
                camera_pitch_deg,
            ),
            &self.transform,
        )
    }

    /// This draw's packed sky/block light byte **if** it carries a brightness
    /// override, in this renderer's `sky << 4 | block` layout — `None` when the
    /// caller must sample the world instead.
    ///
    /// Vanilla's own display-renderer sky/block light getters take the
    /// override's own nibbles in place of the sampled lightmap whenever it is
    /// not `-1`, per axis; unpacking is vanilla's own light-coords block/sky
    /// helpers
    /// (`packed >> 4 & 15` and `packed >> 20 & 15`) against
    /// its own pack routine's `block << 4 | sky << 20`. The two layouts differ —
    /// vanilla's is a 32-bit lightmap coordinate and ours is one byte — so this
    /// is a repack, not a passthrough.
    #[must_use]
    pub fn override_light(&self) -> Option<u8> {
        let packed = self.brightness_override?;
        if packed == NO_BRIGHTNESS_OVERRIDE {
            return None;
        }
        let block = (packed >> 4) & 15;
        let sky = (packed >> 20) & 15;
        Some(((sky << 4) | block) as u8)
    }
}

/// Vanilla's own "no brightness override" sentinel — the value its own
/// brightness-override accessor
/// carries when no `brightness` NBT tag is set.
pub const NO_BRIGHTNESS_OVERRIDE: i32 = -1;

/// The world-space axis-aligned bounds of the unit cube `0..1` carried through
/// `placement`, for a frustum test.
///
/// All eight corners are transformed rather than just two: `placement` carries
/// an arbitrary rotation, so the transformed min/max of the two extreme corners
/// alone is **not** the transformed box's bounds. A display's transformation can
/// also scale by any amount, which is why no fixed one-block slack (what a
/// falling block and an item frame use) is right here.
#[must_use]
pub fn placement_bounds(placement: &glam::Mat4) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8u32 {
        let corner = Vec3::new(
            (i & 1) as f32,
            ((i >> 1) & 1) as f32,
            ((i >> 2) & 1) as f32,
        );
        let p = placement.transform_point3(corner);
        min = min.min(p);
        max = max.max(p);
    }
    (min, max)
}

/// Extracted [`DisplayDraw`]s for this frame — cleared and repopulated by
/// [`extract_display_draws`] every `Extract` pass, mirroring
/// `lodestone_shell::entities::ExtractedDraws` exactly.
#[derive(Debug, Default, Resource)]
struct ExtractedDisplayDraws(Vec<DisplayDraw>);

/// `Extract` / `ExtractSet::Entities`: `Display*` ingest components → the
/// plain [`DisplayDraw`] PODs this module's consumers read.
///
/// Queries the ingest entity directly — see the module doc for why this
/// needs no [`lodestone_ecs::entity::EntityIndex`] bridge the way
/// `extract_entity_draws` needs for its own ingest-only fields
/// (`FallingBlockState`, `HurtTime`, …): there is no separate render-side
/// track entity here to bridge *from*.
fn extract_display_draws(
    query: Query<(
        &MinecraftEntityId,
        &EntityKind,
        &Position,
        &Rotation,
        Option<&DisplayBillboard>,
        Option<&DisplayTranslation>,
        Option<&DisplayScale>,
        Option<&DisplayLeftRotation>,
        Option<&DisplayRightRotation>,
        (
            Option<&DisplayText>,
            Option<&DisplayLineWidth>,
            Option<&DisplayBackgroundColor>,
            Option<&DisplayTextOpacity>,
            Option<&DisplayStyleFlags>,
        ),
        Option<&DisplayBlockState>,
        Option<&DisplayItem>,
        Option<&DisplayItemContext>,
        Option<&DisplayBrightness>,
    )>,
    mut out: ResMut<ExtractedDisplayDraws>,
) {
    out.0.clear();
    for (
        id,
        kind,
        position,
        rotation,
        billboard,
        translation,
        scale,
        left_rotation,
        right_rotation,
        (text, line_width, background_color, opacity, style_flags),
        block_state,
        item,
        item_context,
        brightness,
    ) in &query
    {
        let type_path = match kind.0.path() {
            TEXT_DISPLAY_TYPE_PATH => TEXT_DISPLAY_TYPE_PATH,
            ITEM_DISPLAY_TYPE_PATH => ITEM_DISPLAY_TYPE_PATH,
            BLOCK_DISPLAY_TYPE_PATH => BLOCK_DISPLAY_TYPE_PATH,
            _ => continue,
        };
        let pos = position.0;
        let transform = lodestone_render::display::DisplayTransformation {
            translation: translation.map_or(Vec3::ZERO, |t| Vec3::new(t.0.x, t.0.y, t.0.z)),
            left_rotation: left_rotation
                .map_or(Quat::IDENTITY, |q| Quat::from_xyzw(q.0.x, q.0.y, q.0.z, q.0.w)),
            scale: scale.map_or(Vec3::ONE, |s| Vec3::new(s.0.x, s.0.y, s.0.z)),
            right_rotation: right_rotation
                .map_or(Quat::IDENTITY, |q| Quat::from_xyzw(q.0.x, q.0.y, q.0.z, q.0.w)),
        };
        out.0.push(DisplayDraw {
            id: id.0,
            type_path,
            position: Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32),
            entity_yaw: rotation.0.yaw,
            entity_pitch: rotation.0.pitch,
            billboard: billboard.map_or(lodestone_render::display::BillboardMode::Fixed, |b| {
                lodestone_render::display::BillboardMode::from_wire(b.0)
            }),
            transform,
            // `DisplayText` carries the styled component tree (see its own
            // doc) straight through — `gpu/display_text.rs` reads it with
            // `Text::to_spans` directly, so colour/bold/italic/underline/
            // strikethrough (hex included) survive with no legacy-string
            // round trip in between.
            text: text.map(|t| t.0.clone()),
            text_line_width: line_width.map_or(DEFAULT_LINE_WIDTH, |w| w.0),
            text_background_color: background_color.map_or(DEFAULT_BACKGROUND_COLOR, |c| c.0),
            text_opacity: opacity.map_or(DEFAULT_TEXT_OPACITY, |o| o.0),
            text_style_flags: style_flags.map_or(0, |f| f.0),
            block_state: block_state.map(|s| s.0),
            item: item.and_then(|i| i.0.clone()),
            item_display_context: item_context.map_or(DEFAULT_ITEM_DISPLAY_CONTEXT, |c| c.0),
            brightness_override: brightness.map(|b| b.0),
        });
    }
}

/// This frame's extracted display-entity snapshot, for a render-side
/// consumer — mirrors `lodestone_shell::entities::extracted_entity_draws`.
#[must_use]
pub fn extracted_display_draws(world: &bevy_ecs::world::World) -> Vec<DisplayDraw> {
    world
        .get_resource::<ExtractedDisplayDraws>()
        .map(|res| res.0.clone())
        .unwrap_or_default()
}

/// Installs [`extract_display_draws`]. Added alongside
/// `crate::entities::EntityInterpPlugin` in `Sim::client_app` — see that
/// call site for why this is its own plugin rather than a system folded into
/// `EntityInterpPlugin` (this family has no render-side track to join).
#[derive(Debug, Default)]
pub struct DisplayEntityPlugin;

impl Plugin for DisplayEntityPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ExtractedDisplayDraws>();
        app.add_systems(Extract, extract_display_draws.in_set(ExtractSet::Entities));
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::world::World;
    use lodestone_ecs::CorePlugin;
    use lodestone_ecs::app::App;
    use lodestone_ecs::entity::DisplayBrightness;
    use lodestone_model::{Quat as ModelQuat, Rotation as ModelRotation, Vec3 as ModelVec3, Vec3f};

    use super::*;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((CorePlugin, DisplayEntityPlugin));
        app
    }

    fn run_extract(app: &mut App) -> Vec<DisplayDraw> {
        app.world_mut().run_schedule(Extract);
        extracted_display_draws(app.world())
    }

    /// A `DisplayDraw` at vanilla's own accessor defaults, for the pose gates
    /// below — the same shape `extract_display_draws` produces for an entity
    /// that has reported nothing yet.
    fn a_draw(type_path: &'static str) -> DisplayDraw {
        DisplayDraw {
            id: 1,
            type_path,
            position: Vec3::ZERO,
            entity_yaw: 0.0,
            entity_pitch: 0.0,
            billboard: lodestone_render::display::BillboardMode::Fixed,
            transform: lodestone_render::display::DisplayTransformation::default(),
            text: None,
            text_line_width: DEFAULT_LINE_WIDTH,
            text_background_color: DEFAULT_BACKGROUND_COLOR,
            text_opacity: DEFAULT_TEXT_OPACITY,
            text_style_flags: 0,
            block_state: None,
            item: None,
            item_display_context: DEFAULT_ITEM_DISPLAY_CONTEXT,
            brightness_override: None,
        }
    }

    fn spawn_display(world: &mut World, id: i32, type_path: &str) -> bevy_ecs::entity::Entity {
        world
            .spawn((
                MinecraftEntityId(id),
                EntityKind(type_path.parse().expect("valid entity type path")),
                Position(ModelVec3::new(1.0, 2.0, 3.0)),
                Rotation(ModelRotation::new(45.0, 10.0)),
            ))
            .id()
    }

    /// A negative control: a non-`Display` entity (a plain `pig`, spawned
    /// with the exact same component shape) must never appear in the
    /// extracted list — proves the type-path switch actually filters rather
    /// than passing everything through.
    #[test]
    fn a_non_display_entity_is_never_extracted() {
        let mut app = test_app();
        spawn_display(app.world_mut(), 1, "minecraft:pig");
        let draws = run_extract(&mut app);
        assert!(
            draws.is_empty(),
            "a plain pig must never reach the display-entity extract: {draws:?}"
        );
    }

    /// The positive control paired with the negative one above: a real
    /// `text_display` with no `Display*` components yet (the state right
    /// after `AddEntity`, before any `set_entity_data` has arrived) must
    /// still be extracted, with every field at vanilla's own accessor
    /// default — proving the defaults, not just the filter, are wired.
    #[test]
    fn a_freshly_spawned_text_display_is_extracted_with_vanilla_defaults() {
        let mut app = test_app();
        spawn_display(app.world_mut(), 2, "minecraft:text_display");
        let draws = run_extract(&mut app);
        assert_eq!(draws.len(), 1, "the text_display must be extracted: {draws:?}");
        let draw = &draws[0];
        assert_eq!(draw.type_path, TEXT_DISPLAY_TYPE_PATH);
        assert_eq!(draw.billboard, lodestone_render::display::BillboardMode::Fixed);
        assert_eq!(
            draw.transform,
            lodestone_render::display::DisplayTransformation::default()
        );
        assert_eq!(draw.text, None);
        assert_eq!(draw.text_line_width, DEFAULT_LINE_WIDTH);
        assert_eq!(draw.text_background_color, DEFAULT_BACKGROUND_COLOR);
        assert_eq!(draw.text_opacity, DEFAULT_TEXT_OPACITY);
        assert_eq!(draw.block_state, None);
        assert_eq!(draw.item, None);
    }

    /// Every `Display*` component present, on every subtype's own field —
    /// the "field declared once, read on every variant" shape this repo's
    /// evidence standards call out (a shield's dropped `special`-node
    /// transformation is the incident this test's shape guards against).
    #[test]
    fn a_fully_reported_block_display_carries_every_transform_field() {
        let mut app = test_app();
        let entity = spawn_display(app.world_mut(), 3, "minecraft:block_display");
        app.world_mut().entity_mut(entity).insert((
            DisplayBillboard(3), // Center
            DisplayTranslation(Vec3f::new(0.5, 0.25, -0.5)),
            DisplayScale(Vec3f::new(2.0, 2.0, 2.0)),
            DisplayLeftRotation(ModelQuat::new(0.0, 0.7071, 0.0, 0.7071)),
            DisplayRightRotation(ModelQuat::IDENTITY),
            DisplayBlockState(42),
        ));
        let draws = run_extract(&mut app);
        assert_eq!(draws.len(), 1);
        let draw = &draws[0];
        assert_eq!(draw.type_path, BLOCK_DISPLAY_TYPE_PATH);
        assert_eq!(draw.billboard, lodestone_render::display::BillboardMode::Center);
        assert_eq!(draw.transform.translation, Vec3::new(0.5, 0.25, -0.5));
        assert_eq!(draw.transform.scale, Vec3::new(2.0, 2.0, 2.0));
        assert_eq!(draw.block_state, Some(42));
    }

    /// A `block_display`'s state and an `item_display`'s stack both reach the
    /// snapshot, and the brightness override reaches it off **either** — it is
    /// declared on the base `Display` class, so reading it only on the variant
    /// whose renderer prompted the port is the inherited-field mistake this
    /// repo has already shipped once (a shield's model transformation).
    #[test]
    fn the_brightness_override_is_read_off_every_subtype_not_just_one() {
        for (type_path, expected) in [
            ("minecraft:block_display", BLOCK_DISPLAY_TYPE_PATH),
            ("minecraft:item_display", ITEM_DISPLAY_TYPE_PATH),
            ("minecraft:text_display", TEXT_DISPLAY_TYPE_PATH),
        ] {
            let mut app = test_app();
            let entity = spawn_display(app.world_mut(), 7, type_path);
            app.world_mut()
                .entity_mut(entity)
                .insert(DisplayBrightness((7 << 4) | (12 << 20)));
            let draws = run_extract(&mut app);
            assert_eq!(draws.len(), 1, "{type_path} was not extracted");
            assert_eq!(draws[0].type_path, expected);
            assert_eq!(
                draws[0].brightness_override,
                Some((7 << 4) | (12 << 20)),
                "{type_path} dropped a field declared on the base Display class"
            );
        }
    }

    /// The override unpacks to this renderer's own light byte, and the two
    /// nibbles do not swap.
    ///
    /// # Both hypotheses are computed, not just the right one
    ///
    /// Vanilla packs `block << 4 | sky << 20`; this renderer's byte is
    /// `sky << 4 | block`. The fixture is `Brightness(block 7, sky 12)`, so the
    /// correct answer is `0xC7` and the *swapped* one is `0x7C` — two different
    /// numbers, which a symmetric `(15, 15)` fixture could not have separated.
    /// A raw passthrough of the packed int is a third hypothesis and is
    /// excluded by the byte width alone.
    #[test]
    fn the_brightness_override_unpacks_without_swapping_sky_and_block() {
        let mut draw = a_draw(BLOCK_DISPLAY_TYPE_PATH);
        draw.brightness_override = Some((7 << 4) | (12 << 20));

        let correct = (12 << 4) | 7; // sky 12 in the high nibble, block 7 low
        let swapped = (7 << 4) | 12;
        assert_ne!(correct, swapped, "the fixture cannot discriminate");
        assert_eq!(draw.override_light(), Some(correct));

        // `-1` is vanilla's own no-override sentinel and must read as "sample
        // the world", not as a light value. Unpacked naively it would be
        // `sky 15, block 15` — full bright — which is a plausible-looking wrong
        // answer rather than an obviously broken one.
        draw.brightness_override = Some(NO_BRIGHTNESS_OVERRIDE);
        assert_eq!(draw.override_light(), None);
        draw.brightness_override = None;
        assert_eq!(draw.override_light(), None);

        // And a real all-zero override is *not* the sentinel: it packs to 0.
        draw.brightness_override = Some(0);
        assert_eq!(draw.override_light(), Some(0));
    }

    /// `placement` puts local `(0,0,0)` exactly on the entity position for an
    /// untransformed display — and the falling block's `translate(-0.5, 0, -0.5)`,
    /// the nearest wrong hypothesis on this seam, is half a block away.
    ///
    /// A `block_display` model is block-local `0..1` and its entity position is
    /// the model's own origin (vanilla's own block-display render-substate
    /// update applies no
    /// offset, and neither does its own inner-submit routine), unlike a falling block, whose
    /// entity spawns at the cell *centre*. Borrowing the falling block's shift
    /// would leave every hologram plausibly-but-wrongly placed.
    #[test]
    fn an_untransformed_placement_puts_local_origin_on_the_entity_position() {
        // Both signs, and three distinct axes, so an axis swap cannot pass.
        for position in [Vec3::new(4.0, 70.0, 9.0), Vec3::new(-8.0, -13.0, -3.0)] {
            let mut draw = a_draw(BLOCK_DISPLAY_TYPE_PATH);
            draw.position = position;
            // A camera pointed somewhere arbitrary: `Fixed` must ignore it.
            let pose = draw.placement(137.0, -22.0);
            let origin = pose.transform_point3(Vec3::ZERO);
            let far = pose.transform_point3(Vec3::ONE);
            assert!(
                (origin - position).length() < 1e-5,
                "local origin landed at {origin}, not on the entity at {position}"
            );
            assert!(
                (far - (position + Vec3::ONE)).length() < 1e-5,
                "the far corner landed at {far}, so the pose is not unit-scale"
            );

            // The wrong hypothesis, evaluated rather than described.
            let falling_block_shift = position - Vec3::new(0.5, 0.0, 0.5);
            assert!(
                (falling_block_shift - origin).length() > 0.4,
                "the falling block's -0.5 shift is within 0.4 blocks of the correct \
                 origin, so this gate cannot tell the two apart"
            );
        }
    }

    /// `Fixed` ignores the camera and `Center` tracks it — the minimum pair
    /// that separates a billboard mode being read from one being defaulted.
    ///
    /// Two camera angles, because one proves nothing: a placement that ignored
    /// the mode entirely would be constant across cameras for *both*.
    #[test]
    fn a_fixed_placement_ignores_the_camera_and_a_center_one_does_not() {
        let mut fixed = a_draw(BLOCK_DISPLAY_TYPE_PATH);
        fixed.billboard = lodestone_render::display::BillboardMode::Fixed;
        let mut center = fixed.clone();
        center.billboard = lodestone_render::display::BillboardMode::Center;

        let probe = |draw: &DisplayDraw, yaw, pitch| {
            draw.placement(yaw, pitch).transform_point3(Vec3::ONE)
        };
        assert!(
            (probe(&fixed, 10.0, 5.0) - probe(&fixed, 200.0, -35.0)).length() < 1e-5,
            "a Fixed display moved when only the camera rotated"
        );
        assert!(
            (probe(&center, 10.0, 5.0) - probe(&center, 200.0, -35.0)).length() > 0.5,
            "a Center display did not track the camera"
        );
    }

    /// `placement_bounds` transforms all eight corners, so a rotated box's
    /// bounds are not the transformed corner pair.
    ///
    /// The discriminating input is a **45° roll about Z**, not the 45° yaw a
    /// first attempt used: under a yaw the cube's own `(0,0,0)`→`(1,1,1)`
    /// diagonal projects onto x as `sqrt(2)` too, so the two readings coincide
    /// exactly and the gate proves nothing. Rolled about Z, the four corners
    /// that set the x extent are `(1,0)` and `(0,1)` — neither of which the
    /// two-corner reading touches — so the true width is `sqrt(2)` and the
    /// naive one is **0**. Both numbers are computed here, so this is a
    /// prediction rather than a sign check.
    #[test]
    fn placement_bounds_covers_a_rotated_box_not_just_two_corners() {
        let mut draw = a_draw(BLOCK_DISPLAY_TYPE_PATH);
        draw.position = Vec3::ZERO;
        draw.transform.left_rotation =
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
        let pose = draw.placement(0.0, 0.0);
        let (min, max) = placement_bounds(&pose);
        let width = max.x - min.x;
        assert!(
            (width - std::f32::consts::SQRT_2).abs() < 1e-4,
            "a 45-degree rolled box measured {width} wide; the true bound is sqrt(2)"
        );

        let naive = (pose.transform_point3(Vec3::ZERO), pose.transform_point3(Vec3::ONE));
        let naive_width = (naive.1.x - naive.0.x).abs();
        assert!(
            naive_width < 1e-4,
            "the two-corner reading measured {naive_width}; it must be 0 here, or this \
             gate cannot tell the two readings apart"
        );
    }

    /// An out-of-range billboard byte must fall back to `Fixed` rather than
    /// panicking or silently misreading — `ByIdMap.OutOfBoundsStrategy.ZERO`,
    /// ported by `BillboardMode::from_wire`. Exercised here (not just in
    /// `lodestone-render`'s own unit test) because this is the call site
    /// that would otherwise need its own `match` and could get the fallback
    /// wrong independently.
    #[test]
    fn an_out_of_range_billboard_byte_falls_back_to_fixed_rather_than_panicking() {
        let mut app = test_app();
        let entity = spawn_display(app.world_mut(), 4, "minecraft:item_display");
        app.world_mut().entity_mut(entity).insert(DisplayBillboard(200));
        let draws = run_extract(&mut app);
        assert_eq!(draws[0].billboard, lodestone_render::display::BillboardMode::Fixed);
    }
}
