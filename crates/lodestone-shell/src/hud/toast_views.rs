use super::*;

/// Toast-local strings and sprites are kept as public HUD constants because
/// the app layer owns queueing and supplies the corresponding view records.
pub const RECIPE_TOAST_TITLE: &str = "New Recipe(s) Unlocked!";
pub const RECIPE_TOAST_DESCRIPTION: &str = "Check your recipe book";
pub const RECIPE_TOAST_SPRITE: &str = "toast/recipe";
pub const FRIENDS_TOAST_SPRITE: &str = "friends/toast_background";
pub const RECIPE_TOAST_SLIDE_MS: u64 = 600;

/// One recipe-unlock toast to draw this frame, resolved from
/// [`lodestone_game::recipe::RecipeToastQueue::displayed_entry`] by whoever owns
/// the clock (`app.rs`).
///
/// # Geometry, read from the record rather than a call site
///
/// Every number below comes from vanilla's toast base/vanilla's recipe-toast rendering in
/// `.cache/mc/26.2/client-src`, checked against the **definitions**:
///
/// - vanilla's own toast base's width accessor is 160, its height accessor is 32 (vanilla's own toast base; the
///   `DEFAULT_WIDTH`/`SLOT_HEIGHT` constants at `:14-15` carry the same values).
/// - `xPos(screenWidth, visiblePortion) == screenWidth - width() *
///   visiblePortion`. This is **not** a fixed right
///   margin: it is the slide-in, and at `visiblePortion == 1.0` the toast's
///   left edge sits exactly `160` from the right edge of the screen.
/// - `yPos(firstSlotIndex) == firstSlotIndex * height()`,
///   so the *first* toast is flush with the top of the screen at `y == 0`, not
///   inset by a margin. We only ever draw one, so `firstSlotIndex == 0`.
/// - Contents (the recipe toast's extract render state, vanilla's own recipe-toast rendering), all
///   toast-local: background sprite over the full `160×32`; title at `(30, 7)`
///   colour `-11534256` (`0xFF500050`); description at `(30, 18)` colour
///   `-16777216` (opaque black); the crafting-station icon at `(3, 3)` under a
///   `scale(0.6)` that applies to the *position too*, so it lands at
///   `(1.8, 1.8)` at `9.6px`; the unlocked item's icon at `(8, 8)`, unscaled.
#[derive(Debug, Clone)]
pub struct RecipeToastView {
    /// The crafting station's icon — the small scaled corner item
    /// (the recipe toast's entry::category_item, vanilla's own recipe-toast rendering).
    pub station: ItemIcon,
    /// The newly unlocked recipe's result icon (Entry's unlocked item).
    pub unlocked: ItemIcon,
    /// The toast manager's toast instance::visiblePortion (vanilla's own toast-manager type,
    /// used at `:266`): `1.0` fully on screen, `0.0` entirely off the right
    /// edge. Callers with no animation state should pass `1.0`.
    pub visible_portion: f32,
}

/// `toast/advancement`, the completion toast's background sprite.
pub const ADVANCEMENT_TOAST_SPRITE: &str = "toast/advancement";

/// One advancement-completion toast.
///
/// The advancement toast's extract render state, the
/// single-title-line branch: the type's own heading at `(30, 7)` in yellow — or
/// `0xFFFF88FF` for a challenge — the advancement's title at `(30, 18)` in white,
/// and its icon at `(8, 8)` unscaled, all over the same `160×32` background
/// [`RecipeToastView`] uses.
///
/// **The multi-line branch is not modelled.** Vanilla alternates between the
/// heading and the wrapped title every 1500 ms when the title does not fit 125 px;
/// every one of 26.2's own 126 titles does fit, so the alternation is unreachable
/// with the shipped data pack and a title longer than that degrades to its first
/// line rather than growing a second animation clock.
#[derive(Debug, Clone)]
pub struct AdvancementToastView {
    /// "Advancement Made!" / "Goal Reached!" / "Challenge Complete!", resolved.
    pub heading: String,
    /// The heading's colour — challenge advancements get their own.
    pub heading_colour: [f32; 4],
    /// The advancement's own title, resolved.
    pub title: String,
    /// Its icon, `None` for an id the atlas key parser rejects.
    pub icon: Option<ItemIcon>,
    /// See [`RecipeToastView::visible_portion`].
    pub visible_portion: f32,
}

/// One Friends-service notification using the shared top-right toast slot.
#[derive(Debug, Clone)]
pub struct FriendsToastView {
    /// Profile-less service message, wrapped to the toast's text column.
    pub message: String,
    /// Horizontal slide fraction; `1.0` is fully visible.
    pub visible_portion: f32,
}

/// The recipe-unlock toast's rect in **logical canvas pixels**, as
/// `(x, y, w, h)` — `Toast::xPos`/`yPos` with `firstSlotIndex == 0`.
///
/// This exists so the draw and any gate measuring it share **one** expression.
/// A gate that restated `canvas_w - 160.0` would silently stop describing the
/// draw the moment the slide-in is threaded through, which is exactly the
/// failure mode a HUD gate here already hit once by hardcoding a `cluster_top`
/// the draw computed from a moving anchor.
#[must_use]
pub fn recipe_toast_rect(canvas_w: f32, visible_portion: f32) -> (f32, f32, f32, f32) {
    let tw = lodestone_game::recipe::RECIPE_TOAST_WIDTH as f32;
    let th = lodestone_game::recipe::RECIPE_TOAST_HEIGHT as f32;
    (canvas_w - tw * visible_portion, 0.0, tw, th)
}

/// Gate for the recipe-unlock toast draw.
///
/// The toast timing (`RecipeToastQueue`) landed unit-tested in `lodestone-game`
/// and reached zero pixels because `hud.rs` never rendered it. This measures the
/// draw, in the rect vanilla's own toast base itself specifies.
#[cfg(test)]
mod recipe_toast_gate {
    use super::*;

    /// A frame with the debug overlay and crosshair off, so the **only** thing
    /// that can paint is the toast.
    ///
    /// This matters: a control asserting "nothing paints here" is worthless if
    /// something else already does, and this repo has burned a cycle on exactly
    /// that (a sky control that failed at 3.5% because the first-person bare arm
    /// was drawing, a premise false since long before the feature existed). The
    /// `no_toast_frame_paints_nothing_in_the_toast_rect` test below *verifies*
    /// this premise rather than assuming it.
    fn bare_frame<'a>(stats: &'a DebugStats) -> HudFrame<'a> {
        let mut f = HudFrame::new(stats);
        f.show_debug = false;
        f.crosshair = false;
        f
    }

    fn icon(name: &str) -> ItemIcon {
        ItemIcon {
            item: lodestone_assets::ResourceLocation::parse(name).expect("valid id"),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            custom_model_data: None,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: None,
            skin: None,
        }
    }

    const W: u32 = 640;
    const H: u32 = 480;

    /// The toast rect in NDC, from [`recipe_toast_rect`] — **the same expression
    /// the draw calls**, never a restated `canvas_w - 160.0`.
    fn toast_rect_ndc(visible_portion: f32) -> (f32, f32, f32, f32) {
        let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, W, H);
        let (x, y, tw, th) = recipe_toast_rect(cw, visible_portion);
        (
            2.0 * x / cw - 1.0,
            1.0 - 2.0 * (y + th) / ch,
            2.0 * (x + tw) / cw - 1.0,
            1.0 - 2.0 * y / ch,
        )
    }

    /// **The control's premise, verified rather than assumed**: with no toast,
    /// nothing at all paints in the toast's rect.
    ///
    /// If this ever fails, every "the toast drew" assertion below is measuring
    /// somebody else's pixels and must be re-derived.
    #[test]
    fn no_toast_frame_paints_nothing_in_the_toast_rect() {
        let stats = DebugStats::default();
        let geo = HudGeometry::build(&bare_frame(&stats), W, H);
        let (covered, inside, bbox) = coverage(&geo.verts, toast_rect_ndc(1.0), 96);
        assert!(inside > 0, "the toast rect must contain sample points");
        assert_eq!(
            covered, 0,
            "something other than the toast already paints the top-right \
             {inside}-cell rect (bbox {bbox:?}) — the positive gate's premise is \
             false and its rect must be re-derived"
        );
    }

    /// The toast covers its own rect — vanilla's own toast base's `xPos`/`yPos`/`width`/
    /// `height`, at rest.
    #[test]
    fn a_recipe_toast_covers_toast_javas_own_rect() {
        let stats = DebugStats::default();
        let mut frame = bare_frame(&stats);
        frame.recipe_toast = Some(RecipeToastView {
            station: icon("minecraft:crafting_table"),
            unlocked: icon("minecraft:torch"),
            visible_portion: 1.0,
        });
        let geo = HudGeometry::build(&frame, W, H);
        let rect = toast_rect_ndc(1.0);
        let (covered, inside, bbox) = coverage(&geo.verts, rect, 96);
        let fraction = covered as f32 / inside as f32;
        assert!(
            fraction > 0.9,
            "the toast must fill its rect: covered {covered}/{inside} \
             ({fraction:.3}) in rect {rect:?}, covered bbox {bbox:?}"
        );
    }

    /// The toast is anchored to the **right edge and the very top**, not inset
    /// by a margin.
    ///
    /// This is the assertion that catches a transcription of `yPos` as "some
    /// top margin": `yPos(firstSlotIndex) == firstSlotIndex * height()`
    ///, and with one toast `firstSlotIndex == 0`, so the
    /// top edge is `y == 0` exactly. Predicted from the definition, not the
    /// call site.
    #[test]
    fn the_toast_is_anchored_to_the_top_right_corner() {
        let (cw, _) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, W, H);
        let (x, y, tw, th) = recipe_toast_rect(cw, 1.0);
        assert_eq!(y, 0.0, "the first toast sits flush with the top of the screen");
        assert_eq!(tw, 160.0, "Toast::width()");
        assert_eq!(th, 32.0, "Toast::height()");
        assert_eq!(
            x + tw,
            cw,
            "at full visibility the toast's right edge is the screen's right edge"
        );
        // The slide is a *scaling of the width*, so half-visible puts the left
        // edge exactly 80 logical pixels from the right edge — and this differs
        // from the wrong hypothesis (a fixed x that ignores visible_portion),
        // which would still report `cw - 160`.
        let (half_x, _, _, _) = recipe_toast_rect(cw, 0.5);
        assert_eq!(half_x, cw - 80.0, "xPos scales the width by visible_portion");
        assert_ne!(
            half_x,
            cw - 160.0,
            "a fixed-margin transcription would fail to move at all"
        );
    }
}
