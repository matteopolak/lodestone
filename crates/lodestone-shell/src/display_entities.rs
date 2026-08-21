//! The `Display` entity family's draw-ready snapshot: `text_display`/
//! `item_display`/`block_display`, extracted from the ingest ECS into the
//! plain [`DisplayDraw`] PODs `gpu/display_text.rs` consumes.
//!
//! **`item_display`/`block_display` have no GPU consumer yet.** A block/item
//! merge site in `gpu/moving_blocks.rs`/`gpu/world_items.rs` was the planned
//! shape and is not built; every `DisplayDraw` still reaches
//! [`extracted_display_draws`] regardless of `type_path`, and
//! `RenderState::set_display_draws` logs each unsupported one once so the gap
//! is visible rather than a silent no-op.
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
    DisplayBackgroundColor, DisplayBillboard, DisplayBlockState, DisplayItem, DisplayItemContext,
    DisplayLeftRotation, DisplayLineWidth, DisplayRightRotation, DisplayScale, DisplayStyleFlags,
    DisplayText, DisplayTextOpacity, DisplayTranslation, EntityKind, MinecraftEntityId, Position,
    Rotation,
};
use lodestone_ecs::{Extract, ExtractSet};
use lodestone_model::ItemStack;

/// `Display.TextDisplay`'s registry path, as [`DisplayDraw::type_path`]
/// carries it (bare path, no namespace — matching
/// `lodestone_shell::entities::EntityDraw::type_path`'s own convention).
pub const TEXT_DISPLAY_TYPE_PATH: &str = "text_display";
/// `Display.ItemDisplay`'s registry path.
pub const ITEM_DISPLAY_TYPE_PATH: &str = "item_display";
/// `Display.BlockDisplay`'s registry path.
pub const BLOCK_DISPLAY_TYPE_PATH: &str = "block_display";

/// Vanilla's `Display.TextDisplay` accessor defaults, applied wherever the
/// corresponding component has never been reported — matching every default
/// this repo already documents on the `Display*` components themselves
/// (`lodestone_ecs::entity::DisplayLineWidth` etc.).
const DEFAULT_LINE_WIDTH: i32 = 200;
const DEFAULT_BACKGROUND_COLOR: i32 = 0x4000_0000_u32 as i32;
const DEFAULT_TEXT_OPACITY: i8 = -1;
/// `ItemDisplayContext.FIXED`'s ordinal (`ItemDisplayContext.java`, `26.2`):
/// the no-perspective context vanilla itself uses for a context-less item
/// (an item frame's contents, and — per this seam's own simplification —
/// what an `item_display` with no reported context draws in, rather than
/// `NONE` (ordinal `0`), which draws nothing at all.
const DEFAULT_ITEM_DISPLAY_CONTEXT: u8 = ItemDisplayContextOrdinal::FIXED;

/// The handful of `ItemDisplayContext` ordinals this seam's consumers care
/// about, named rather than left as bare integers at every call site —
/// transcribed from `ItemDisplayContext.java`'s enum declaration order
/// (`NONE, THIRDPERSON_LEFTHAND, THIRDPERSON_RIGHTHAND,
/// FIRSTPERSON_LEFTHAND, FIRSTPERSON_RIGHTHAND, HEAD, GUI, GROUND, FIXED, …`).
struct ItemDisplayContextOrdinal;
impl ItemDisplayContextOrdinal {
    const FIXED: u8 = 8;
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
    /// The entity's own reported yaw, degrees — `Display.BillboardConstraints::FIXED`'s
    /// yaw source (`Entity.getYRot`). Irrelevant to every other billboard
    /// mode, which reads the *camera's* yaw instead — see
    /// `lodestone_render::display::display_orientation`.
    pub entity_yaw: f32,
    /// The entity's own reported pitch, degrees — `Fixed`'s pitch source.
    pub entity_pitch: f32,
    /// `Display.BillboardConstraints`, resolved from the raw wire ordinal —
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
    /// Legacy-`§`-coded through [`lodestone_model::Text::to_legacy_string`],
    /// not a fully flattened plain string — see this struct's construction
    /// site for why the encoding stops there rather than carrying
    /// [`lodestone_model::Text`] itself.
    pub text: Option<String>,
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
    /// unreported — **not** `NONE` (ordinal `0`), which draws nothing.
    pub item_display_context: u8,
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
            // `DisplayText` now carries the styled component tree (see its
            // own doc); `DisplayDraw::text` stays a plain `String` because
            // `gpu/display_text.rs`'s existing styled-draw bridge is
            // `Text::from_legacy(text).to_spans()` (out of scope for this
            // change — see its own module doc). `to_legacy_string` is the
            // encoding that bridge reads and carries colour/bold/italic/
            // underline/strikethrough through it intact; a *hex* colour is
            // the one documented gap `to_legacy_string` itself cannot close.
            text: text.map(|t| t.0.to_legacy_string()),
            text_line_width: line_width.map_or(DEFAULT_LINE_WIDTH, |w| w.0),
            text_background_color: background_color.map_or(DEFAULT_BACKGROUND_COLOR, |c| c.0),
            text_opacity: opacity.map_or(DEFAULT_TEXT_OPACITY, |o| o.0),
            text_style_flags: style_flags.map_or(0, |f| f.0),
            block_state: block_state.map(|s| s.0),
            item: item.and_then(|i| i.0.clone()),
            item_display_context: item_context.map_or(DEFAULT_ITEM_DISPLAY_CONTEXT, |c| c.0),
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
