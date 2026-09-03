//! The frame data model: [`Align`], [`MenuLabel`], [`MenuRow`],
//! [`MenuNotice`], [`AccountEntryView`], [`ServerEntryView`], [`MenuFrame`]
//! and the [`FaviconCache`] that feeds it, plus [`owns_frame`].
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

/// Horizontal alignment of a [`MenuLabel`] about its anchored x.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// `x` is the text's left edge.
    Left,
    /// `x` is the text's centre.
    Centre,
    /// `x` is the text's right edge. The width is measured at draw time, which
    /// is why this is an alignment and not a pre-computed offset: vanilla's own
    /// `copyrightX = width - font.width(text) - 2`
    /// depends on the font, and the font is not known until the draw.
    Right,
}

/// A free-standing string a vanilla-laid-out screen draws, outside any widget.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuLabel {
    /// The text.
    pub text: String,
    /// Anchor the position is measured from.
    pub origin: Origin,
    /// Horizontal offset from the anchor, before [`Self::align`] is applied.
    pub dx: f32,
    /// Vertical offset from the anchor — the **top** of the line.
    pub dy: f32,
    /// How `dx` relates to the text's own box.
    pub align: Align,
    /// RGBA, sRGB 0..1 written verbatim (the shell's convention — see
    /// `docs/vanilla-hud-text.md`).
    pub colour: [f32; 4],
    /// Font-pixel scale. `1.0` for ordinary vanilla component text (every
    /// label before issue #103 used this implicitly — `build`'s `frame.vanilla`
    /// loop hardcoded it). The death screen's title needs `2.0`:
    /// `DeathScreen.visitText` sets `output.defaultParameters(normalParameters.
    /// withScale(2.0F))` before drawing it.
    pub scale: f32,
}

/// One drawable row: a main-menu button, a server, or a form field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuRow {
    /// Primary label, drawn at [`TEXT_SCALE`].
    pub label: String,
    /// Second line (MOTD, address, hint), drawn small and dim.
    pub detail: String,
    /// Right-aligned trailing text (players, latency).
    pub trailing: String,
    /// Favicon to draw at the row's left edge.
    pub favicon: Option<FaviconMosaic>,
    /// A player head to draw at the row's left edge instead of a favicon —
    /// the account list's own icon (issue #66/#62). Drawn through the exact
    /// same [`FaviconMosaic`] path as `favicon`: a head is not a conceptually
    /// different kind of "small square texture", so it gets no second
    /// drawable type or draw call to drift from the favicon one. See
    /// [`default_head_icon`] for why the *texture* is a parameter here
    /// rather than a hardcoded draw, which is what makes swapping in a real
    /// downloaded skin later a data change, not a rewrite.
    pub head: Option<FaviconMosaic>,
    /// Whether the row can be activated (a failed row is still selectable).
    pub enabled: bool,
    /// Draw `detail` in the failure colour.
    pub detail_is_error: bool,
    /// Draw the row as a text-entry field.
    ///
    /// With [`Self::edit`] set this only selects the field *fill* for the
    /// jar-less fallback; the caret, the selection and the visible slice all come
    /// from the widget. Without it, the pre-#395 draw applies: the whole label
    /// with a caret parked after it.
    pub field: bool,
    /// Draw the row's background as vanilla's `AbstractSliderButton` track
    /// instead of a `Button`.
    ///
    /// A settings screen's numeric options are sliders and its enums and
    /// booleans are `CycleButton`s, and the two
    /// look nothing alike — a slider track has no bevel and no disabled variant.
    ///
    /// This used to say "no live option in this client is a slider", citing
    /// `guiScale`'s `ClampingLazyMaxIntRange` — true when written, false since
    /// issue #203 gave `mouseWheelSensitivity` a real live value (see
    /// [`Self::slider_value`]). Kept as its own `bool` rather than folded into
    /// that field because a non-slider row still needs to say "not a slider" and
    /// `Option<f32>` already carries that (`None`); this is `is_slider`, not
    /// `has_a_known_value`.
    pub slider: bool,
    /// The `[0, 1]` fraction along the track where the handle sits —
    /// `AbstractSliderButton.value` —
    /// or `None` when [`Self::slider`] is `true` but this client holds no
    /// value for the option at all yet.
    ///
    /// Meaningless unless [`Self::slider`] is also `true`; nothing reads it
    /// otherwise. See [`super::options::Cell::slider_fraction`] for where a
    /// `Some` comes from — either the real live config value
    /// (`mouseWheelSensitivity`) or vanilla's own default double for a
    /// `UnitDouble`-based option this client does not wire, which is not a
    /// fabricated value: it is the same constant a fresh vanilla install
    /// boots with.
    pub slider_value: Option<f32>,
    /// The live [`EditBox`] this row draws — a **clone**, taken per frame from
    /// [`super::nav::EditForm`]'s persistent widgets.
    ///
    /// This is the one piece of menu state that is not derivable from the screen
    /// (a caret and a scroll offset are not), so the widget outlives the frame
    /// and the frame carries a copy. `build`'s `draw_edit_box` repositions the
    /// copy into this frame's rect — `OptionsSubScreen.repositionElements`'
    /// order, not `rebuildWidgets`' — and then *asks* it for its geometry rather
    /// than restating any of `EditBox`'s arithmetic here. See
    /// [`super::edit_box`] and [`super::nav::EditForm`].
    pub edit: Option<EditBox>,
    /// Vanilla placement. `Some` puts the row at a rect derived from vanilla's
    /// own arithmetic ([`title_slot`] / [`pause_slot`]) and draws it as a real
    /// `widget/button*` nine-slice sprite; `None` keeps the centred row stack
    /// the server list, the edit form, Options and the error screen use.
    pub slot: Option<Slot>,
    /// A GUI sprite id drawn centred in the widget **instead of** `label` —
    /// vanilla's `SpriteIconButton.CenteredIcon`
    ///. `label` is still carried (it is the
    /// tooltip/narration text in vanilla) but not drawn.
    pub icon: Option<&'static str>,
    /// A book reader/editor page-turn control, drawn as vanilla's bare
    /// `widget/page_*` sprite instead of as a rectangular menu button.
    ///
    /// The book sheet already supplies the visual surface behind these 23×13
    /// controls; giving them the ordinary `widget/button*` background both
    /// looks wrong and expands the visual affordance beyond the hit box vanilla
    /// exposes.
    pub book_page: Option<BookPageButton>,
    /// Set on a [`super::Screen::ServerList`] row: everything an
    /// `OnlineServerEntry` draws that a button row has no field for.
    ///
    /// Its presence is what routes the row to [`draw_server_entry`] instead of
    /// [`draw_widget`], *before* the `slot` test — a list entry is not a button
    /// with an icon, it is a different drawable with three text columns and a
    /// hover overlay. `label` (the server's name) and `favicon` are read from the
    /// row itself rather than duplicated in here.
    pub entry: Option<ServerEntryView>,
    /// Set on a [`super::Screen::Accounts`] list row: the little an account row
    /// needs beyond `label`/`detail`/`trailing`/`head`, which it reads off the
    /// row itself exactly as a multiplayer entry reads `label`/`favicon`.
    ///
    /// Its presence routes the row to [`draw_account_entry`] and, in
    /// [`row_rect`], to [`accounts_row_rect`] — both tested *before* `slot`, for
    /// [`Self::entry`]'s reason: a list entry is not a button, and the row column
    /// is `floor(w / 2) - floor(305 / 2)`, which a `Slot` cannot express.
    pub account: Option<AccountEntryView>,
    /// Set on a [`super::Screen::WorldSelect`] **world-list** row (the save
    /// list): which row it is and whether it is the list's selection.
    ///
    /// Routes the row to [`draw_world_entry`] and, in [`row_rect`], to
    /// [`world_list_row_rect`] — both tested *before* `slot`, for [`Self::entry`]'s
    /// reason twice over: a list entry is not a button, and `getRowLeft()` is
    /// `floor(w / 2) - floor(270 / 2)`, two integer divisions a `Slot` cannot
    /// express.
    ///
    /// The three text lines are the row's own `label` / `detail` / `trailing`,
    /// exactly as an account row reads `label`/`detail` off the row rather than
    /// duplicating them here — so this view carries only what has nowhere else to
    /// live.
    pub world: Option<WorldEntryView>,
    /// Set on a [`super::packs::PacksControl::Entry`] row — the Resource Packs
    /// screen's pack entries.
    ///
    /// Its presence routes the row to [`draw_pack_entry`] instead of
    /// [`draw_widget`], which is the **only** thing it decides. Unlike
    /// [`Self::entry`]/[`Self::account`]/[`Self::world`] it is tested *after*
    /// `slot` rather than before, and that is not an inconsistency: those three
    /// lists are positioned by `getRowLeft()`'s two integer divisions, which a
    /// [`Slot`] cannot express, so they need their own [`row_rect`] arm as well.
    /// A pack row's rect **is** a `Slot` ([`super::packs::placement_anchor`]), so
    /// `row_rect` needs no arm and this field is a draw selector alone.
    ///
    /// The row's own `label` (the pack title), `detail` (its `pack.mcmeta`
    /// description) and `favicon` (its `pack.png`) are read off the row, exactly
    /// as the three lists above read theirs — so this view carries only the two
    /// facts that have nowhere else to live.
    ///
    /// **Why this exists at all**: a pack row used to be a plain slotted row, so
    /// it fell through to `draw_widget` and was drawn as a vanilla *button* with
    /// a centred label — its icon and description computed and then discarded.
    /// That was the reported bug, and it was invisible to every test on this
    /// screen because they all assert on frame data, which was correct
    /// throughout.
    pub pack: Option<PackEntryView>,
    /// A small solid triangle drawn centred in the widget **instead of**
    /// `label`, the way [`Self::icon`] draws a sprite instead of one.
    ///
    /// The Resource Packs screen's two per-row reorder buttons. Vanilla has
    /// sprite arrows for these (`transferable_list/move_up`, `move_down`), but
    /// they are 32×32 quadrant overlays for a hover model this client does not
    /// use (see [`super::packs`]'s module doc), so a triangle drawn as geometry
    /// is both closer to what vanilla shows and independent of the font — which
    /// is upper-case 5×7 with no arrow glyph, and is why these buttons were
    /// lettered `"U"`/`"D"` before this existed.
    pub arrow: Option<Arrow>,
    /// Vanilla's `AbstractWidget.setTooltip` text for this row, as one string —
    /// `\n` separated where the source component has explicit breaks, wrapped to
    /// vanilla's own 170 px by the draw.
    ///
    /// **Held here rather than resolved in the draw** because it is a property of the
    /// control, and only the screen's own table knows it: on the settings tree it
    /// comes from the `OptionInstance`'s `TooltipSupplier`, keyed by the option, and
    /// 33 of 143 controls have one. That ratio is vanilla's — most options carry no
    /// tooltip at all — so a `None` here is faithful rather than unfinished.
    ///
    /// The multiplayer list's "who's online" tooltip does **not** travel through this
    /// field, and the difference is not an inconsistency: that one is a property of
    /// the *cursor's position inside* the row (it fires only over the status column,
    /// and its lines are the player sample), so `draw_server_entry` reports it from
    /// the geometry it already has. This field is the whole-widget kind.
    pub tooltip: Option<String>,
    /// Set on one of the Statistics screen's tab-bar rows.
    ///
    /// Its presence routes the row to [`draw_tab`] instead of [`draw_widget`],
    /// tested before `slot` for [`Self::pack`]'s reason: a tab is not a button
    /// with a label in it, it draws a different sprite set keyed by
    /// `(selected, hovered)` rather than `(active, hoveredOrFocused)`, plus an
    /// underline the generic path has no field for. `label` (the tab's own
    /// caption) and `enabled` (vanilla's `active`, `StatsScreen.
    /// setTabActiveStateAndTooltip`) are read off the row itself, exactly as a
    /// pack row reads `label`/`favicon` off `MenuRow` rather than duplicating
    /// them here — this view carries only `selected`, which has nowhere else to
    /// live (it is not [`MenuFrame::selected`]; see [`TabEntryView::selected`]'s
    /// own doc).
    pub tab: Option<TabEntryView>,
}

/// Which of vanilla's two [`PageButton`](BookPageButton) sprites a book row
/// draws. Kept on the row because both drawing and hit-testing already consume
/// the same row geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookPageButton {
    /// `widget/page_backward` (or its highlighted counterpart).
    Backward,
    /// `widget/page_forward` (or its highlighted counterpart).
    Forward,
}

impl MenuRow {
    /// Whether this row belongs to the screen's scrolling list, and must therefore
    /// be **clipped to that list's band** — by the draw, and equally by the
    /// hit-test.
    ///
    /// ## Why this is on the row rather than on the slot
    ///
    /// Because a row can belong to a list in four different ways, and asking only
    /// about the slot sees one of them. [`Origin::is_scrolling_list_row`] was the
    /// whole test for a while, and it left the three `MenuRow::entry`/`account`/
    /// `world` lists unclipped in the hit-test: those rows carry **no** slot at all
    /// (their columns are `getRowLeft()`'s two separate integer divisions, which a
    /// [`Slot`] cannot express), so the slot test simply did not fire and the row
    /// stayed hit-testable everywhere. The reported symptom was on the multiplayer
    /// screen (2026-08-07): with a server row scrolled under the footer, pressing
    /// *Join Server* neither highlighted the button nor pressed it — the row won,
    /// because rows and the footer share one flat index space and the first rect
    /// containing the cursor wins.
    ///
    /// The draw and the hit-test must agree about this, and the only way to
    /// guarantee that is for both to call **this** function. The band already had
    /// that property — it comes from `frame.list` through
    /// [`widget::ListSpec::model`] in both places — and row membership did not,
    /// which is precisely why the two disagreed.
    ///
    /// ## Why it is not "every row on a screen that has a list"
    ///
    /// A footer button, a title, a search field and `OptionsScreen`'s own arranged
    /// grid live *outside* the band by construction. Clipping them to it would
    /// erase them, and rejecting them outside it would make them unclickable — the
    /// exact bug, mirrored. See [`Origin::is_scrolling_list_row`] for which
    /// placements are which, and why that match has no `_ =>` arm.
    #[must_use]
    pub fn is_scrolling_list_row(&self) -> bool {
        // The three `AbstractSelectionList` screens, whose rows are placed by
        // `row_rect`'s own arms rather than by a `Slot` — the multiplayer list, the
        // account list and the world/save list.
        self.entry.is_some()
            || self.account.is_some()
            || self.world.is_some()
            // Everything else: a settings-tree list row is a slotted widget, and a
            // resource-pack row is one too (`MenuRow::pack` is a draw selector, not
            // a placement).
            || self
                .slot
                .is_some_and(|slot| slot.origin.is_scrolling_list_row())
    }
}

/// Which way a [`MenuRow::arrow`] triangle points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    /// Apex at the top — "raise priority".
    Up,
    /// Apex at the bottom — "lower priority".
    Down,
}

/// One resource-pack row's state.
///
/// Two fields, for [`WorldEntryView`]'s reason: everything else a pack row draws
/// is already a [`MenuRow`] field. Both of these say what the **hover overlay**
/// over the 32×32 icon is — vanilla's `transferable_list/select` /
/// `unselect` sprites, drawn only when its own show-hover-overlay check passes
/// — and neither is derivable from the
/// row: which column a pack is in is the screen's fact, not the row's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackEntryView {
    /// Vanilla's own pack-selection-model entry's can-select check — this row is in **Available**, so
    /// activating it moves the pack into Selected.
    pub can_select: bool,
    /// `canUnselect()` — this row is a removable **Selected** entry. False for
    /// the built-in pack, which is `isFixedPosition() && isRequired()` and
    /// therefore draws no overlay at all, exactly as vanilla's does not.
    pub can_unselect: bool,
}

/// One tab-bar row's state — the shared tab widget's own view type (issue
/// #564), now with two consumers: Statistics's `TAB_LABELS` and Create New
/// World's own (#567). Both build one of these per tab rather than reaching
/// for a bespoke row type each, which is the whole point of building the
/// widget once — see [`super::layout::tab_bar_row_rect`], the one geometry
/// function [`super::render::row_rect`]'s `MenuRow::tab` arm resolves every
/// tab from, regardless of which screen it belongs to.
///
/// `label` and `enabled` are already [`MenuRow`] fields (the caption and
/// vanilla's `active`), so this carries only what has nowhere else to live —
/// [`WorldEntryView`]'s reason for its own two fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TabEntryView {
    /// This tab's position in the screen's own tab-label list — **not** its
    /// position in [`MenuFrame::rows`], which also carries the screen's other
    /// rows (a Done button, form fields, a footer) and so is offset from this
    /// by however many of those precede the tab bar. The same split
    /// [`WorldEntryView::index`]/[`ServerEntryView`] already make between
    /// "row position in the frame" and "position in the screen's own list".
    pub index: usize,
    /// How many tabs this screen's bar has — [`super::layout::tab_bar_geometry`]'s
    /// `tab_count`, carried on the row itself rather than read back off a
    /// screen-specific constant, which is what lets [`super::render::row_rect`]
    /// resolve a tab's rect without knowing which screen it came from. Every
    /// tab row from the same bar carries the same value; a screen with three
    /// tabs stamps `3` onto all three of its `TabEntryView`s.
    pub count: usize,
    /// Whether this is the tab currently showing — vanilla's own tab-manager
    /// get-current-tab accessor
    /// == this.tab (its own tab-button's is-selected check, in its own tab-button type).
    ///
    /// A different question from [`MenuFrame::selected`], which on every other
    /// screen carries the **keyboard-focused** row: Statistics's tab bar has no
    /// per-tab keyboard focus of its own yet (see `stats.rs`'s module docs on
    /// what is and is not wired), so `selected` here is driven by which tab is
    /// showing, the same split [`WorldEntryView::selected`] already argues for
    /// on the save list. Create New World's tab bar is fully clickable (its
    /// three tabs all have real content, unlike Statistics's Items/Mobs), but
    /// still carries no keyboard tab-order of its own — see `create_world.rs`'s
    /// module doc on that scope cut.
    pub selected: bool,
}

/// One world-list row's state (the save list, reading 2).
///
/// Two fields, for [`AccountEntryView`]'s reason: the display name is the row's
/// `label`, `folder (last played)` is its `detail`, and the game-mode/version
/// info line is its `trailing`. Duplicating any of them here is how a row and its
/// draw end up disagreeing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldEntryView {
    /// The row's index in the **filtered** list, which is what
    /// [`world_list_row_rect`] positions from.
    pub index: usize,
    /// Whether this is vanilla's own world-selection list's get-selected-opt
    /// accessor's entry, which gets
    /// `AbstractSelectionList.extractItem`'s 1 px outline plus black interior.
    ///
    /// A different question from [`MenuFrame::selected`], which on this screen
    /// carries the **focused** row, and from [`MenuFrame::hovered`], which carries
    /// the row the mouse is over. All three are visible at once and mean different
    /// things — the same split `world_select`'s own module doc argues for.
    pub selected: bool,
    /// The list's scroll offset, **in logical pixels**.
    ///
    /// Denormalized onto every entry for exactly [`ServerEntryView::scroll`]'s
    /// reason: [`world_list_row_rect`] is also `app`'s hit-test, so it must resolve
    /// a row's position and visibility from the row alone rather than needing a
    /// second plumbing path from `WorldSelectNav` to the draw. It is also the
    /// number the scrollbar thumb is placed from, so the bar and the rows cannot
    /// read different offsets.
    pub scroll: f32,
}

/// One account-list row's state (issues #66/#402).
///
/// Deliberately two fields. Everything else a row draws is already a [`MenuRow`]
/// field — the username is `label`, "Microsoft account" is `detail`, the
/// "Selected" marker is `trailing`, the head icon is `head` — and duplicating any
/// of them here is how a row and its draw end up disagreeing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountEntryView {
    /// The row's index in the **full** account list.
    ///
    /// **This was the rendered-window index**, because the frame builder used to
    /// slice `rows[scroll..scroll + shown]` and hand out `0..shown`. It is the
    /// logical index now, and [`Self::scroll`] carries the offset instead — the
    /// same shape [`ServerEntryView`] already had, and the change that lets the
    /// list sit at a position that is not a whole multiple of the row height.
    pub index: usize,
    /// The list's scroll offset, **in logical pixels**.
    ///
    /// Denormalized onto every entry for exactly [`ServerEntryView::scroll`]'s
    /// reason: [`accounts_row_rect`] is also `app`'s hit-test, so it must resolve a
    /// row's position from the row alone rather than needing a second plumbing path
    /// from `AccountsNav` to the draw. It is also the number the scrollbar thumb is
    /// placed from, so the bar and the rows cannot read different offsets.
    pub scroll: f32,
    /// Whether the list cursor is on this row — `AccountsNav::highlighted`, which
    /// gets `AbstractSelectionList.extractItem`'s 1 px outline plus black
    /// interior.
    ///
    /// A different question from [`MenuFrame::selected`], which on this screen
    /// carries the **footer button** the mouse is over. Both are visible at once
    /// and are drawn completely differently — the same two-cursor split
    /// `docs/server-list.md` argues for the multiplayer screen.
    pub selected: bool,
}

/// A block of **wrapped, bounded** body text: the account screen's sign-in
/// failure reason, the URL it asks the player to open, and its save-error line.
///
/// ## Why this exists
///
/// A [`MenuLabel`] is one unwrapped line drawn at whatever scale it asks for, and
/// [`MenuFrame::message`] is the same thing centred at [`TEXT_SCALE`]. That is
/// fine for text *we* wrote and whose length we control. It is not fine for text
/// we did not: [`super::accounts::describe_auth_error`] renders an
/// `AuthError`, and several of that type's variants carry a snippet of whatever
/// Microsoft or Mojang actually returned — a few hundred characters of JSON with
/// no whitespace in it. Drawn as one scale-2 centred line, that ran off both
/// edges of the screen, which is what a player reported.
///
/// ## What is carried, and what is not
///
/// The **text**, not the lines. Wrapping has to be measured in the font the draw
/// will use, so it happens at draw time — the same reason
/// [`ServerEntryView::motd`] is carried unwrapped. The line *count* is not
/// carried either: [`Self::bottom`] says how much of the canvas to keep clear and
/// [`notice_rect`] turns that into however many whole [`LINE_H`] lines fit, so the
/// layout decides how much text a canvas shows rather than a constant deciding it
/// for every canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuNotice {
    /// The unwrapped text. May contain `\n`, and may be arbitrarily long.
    ///
    /// Always the plain string, even when [`Self::spans`] is populated: wrapping
    /// is measured in the font at draw time and the wrapper works on characters,
    /// so the styled runs are re-applied to the wrapped lines afterwards (see
    /// `draw`'s `restyle_wrapped`) rather than wrapped themselves.
    pub text: String,
    /// The same content as styled runs, or empty for a notice with no styling.
    ///
    /// A notice whose content is **not ours** — a server's kick message, a
    /// server's response body — carries the sender's own colours, and flattening
    /// them here is how a coloured kick message came out uniformly red. When this
    /// is empty the draw falls back to [`Self::colour`] for the whole block,
    /// which is what every notice this shell authors itself wants.
    pub spans: Vec<TextSpan>,
    /// Anchor the block is measured from.
    pub origin: Origin,
    /// Horizontal offset from the anchor — the block's **left** edge.
    pub dx: f32,
    /// Vertical offset from the anchor — the **top** of the first line.
    pub dy: f32,
    /// The wrap column's width. No line may measure wider than this, including a
    /// line made of a single unbroken word (see [`wrap_bounded`]).
    pub w: f32,
    /// Pixels kept clear at the **bottom of the canvas**. The line count is
    /// `floor((height - bottom - top) / LINE_H)`.
    pub bottom: f32,
    /// RGBA, sRGB 0..1 verbatim — the shell's convention.
    pub colour: [f32; 4],
}

/// The rect a [`MenuNotice`] is bounded to on a `width`×`height` canvas: its wrap
/// column, and as many whole [`LINE_H`] lines as fit above
/// [`MenuNotice::bottom`].
///
/// **Public because the gate reads it.** A test that restated this arithmetic
/// would be asserting its own copy of the layout, which `CLAUDE.md` records as
/// having been wrong twice; this is the expression [`build`] draws from.
#[must_use]
pub fn notice_rect(notice: &MenuNotice, width: f32, height: f32) -> (f32, f32, f32, f32) {
    let (ax, ay) = notice.origin.anchor(width, height);
    let x = (ax + notice.dx).floor();
    let y = ay + notice.dy;
    let room = (height - notice.bottom - y).max(0.0);
    (x, y, notice.w, (room / LINE_H).floor() * LINE_H)
}

/// How many whole lines [`notice_rect`] found room for.
pub(super) fn notice_lines(notice: &MenuNotice, width: f32, height: f32) -> usize {
    let (_, _, _, h) = notice_rect(notice, width, height);
    (h / LINE_H).floor().max(0.0) as usize
}

/// One multiplayer-list row's state, in the form
/// `ServerSelectionList.OnlineServerEntry.extractContent` needs it.
///
/// Everything here is resolved by [`server_list_frame`] — which sprite, which
/// colour, whether the move arrows apply — so the draw decides nothing except
/// where. The one thing it cannot resolve is *hover*, because that depends on the
/// canvas, and the canvas is only known at draw time (see [`MenuFrame::cursor`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerEntryView {
    /// The row's index in the list — vanilla's
    /// `ServerSelectionList.this.children().indexOf(this)`, which is what the
    /// pinging animation's phase and both move arrows key on.
    pub index: usize,
    /// The MOTD, unwrapped and possibly multi-line. Wrapped at draw time,
    /// because the wrap width is measured in the font the draw will use.
    pub motd: String,
    /// The same MOTD as styled runs, when the server sent one.
    ///
    /// Empty for the synthetic MOTDs (`Pinging...`, a connection error), which
    /// have no server styling and draw in [`SERVER_ENTRY_DIM`] /
    /// [`SERVER_ENTRY_BAD`] as before. Wrapping still happens on
    /// [`Self::motd`] — the styles are re-attached to the wrapped lines by
    /// [`restyle_wrapped`], so there is exactly one wrap implementation.
    pub motd_spans: Vec<TextSpan>,
    /// Draw the MOTD in [`SERVER_ENTRY_BAD`] — vanilla's `CANT_RESOLVE_TEXT` /
    /// `CANT_CONNECT_TEXT`, which carry their own red component colour.
    pub motd_is_error: bool,
    /// The right-aligned status column: the player count, or an incompatible
    /// server's version string.
    pub status: String,
    /// Draw `status` in [`SERVER_ENTRY_INCOMPATIBLE`] rather than
    /// [`SERVER_ENTRY_DIM`].
    pub status_is_error: bool,
    /// The lines a "who's online" tooltip draws when the cursor is over this
    /// row's status text — the sample's names plus vanilla's
    /// `... and N more ...` when applicable ([`super::status::player_sample_lines`]).
    ///
    /// Empty means the row has no tooltip: an empty sample, or a state vanilla
    /// never shows one for. Resolved by [`server_list_frame`] exactly like the
    /// other presentation fields; the draw ([`super::draw::draw_server_entry`])
    /// only decides *whether* the cursor is over the status text.
    pub online_players: Vec<String>,
    /// The `server_list/*` sprite for this row's state — see
    /// [`super::status::status_sprite`], which is the only thing that picks one.
    pub status_sprite: &'static str,
    /// Whether this is the list's selected entry (`getSelected() == this`), which
    /// is a different question from [`MenuFrame::selected`]: on this screen that
    /// field carries the *footer button* the cursor is over.
    pub selected: bool,
    /// `index > 0` — vanilla's guard on the move-up arrow (`:375`).
    pub can_move_up: bool,
    /// `index < servers.size() - 1` — the move-down guard (`:386`).
    pub can_move_down: bool,
    /// The list's current scroll offset, **in logical pixels** (issues #402,
    /// #445). Denormalized onto every entry (rather than added as a parameter to
    /// [`row_rect`] and every render function it calls) so `row_rect` — which
    /// `app.rs`'s hit-test reads too — can resolve a row's position and
    /// visibility from the row alone, with no second plumbing path from
    /// `MenuNav` to the draw.
    ///
    /// **Pixels rather than rows since #445**, which is what makes the wheel
    /// scroll by vanilla's 18 px half-entry instead of jumping a whole row. This
    /// is also the value [`server_scroll_list`] hands the scrollbar, so the thumb
    /// and the rows read the same number — see [`server_scroll_model`].
    pub scroll: f32,
}

/// What sits behind a screen's widgets — vanilla's three out-of-world/in-world
/// backdrop shapes, as three states rather than as one boolean.
///
/// # Why this is not a `bool`
///
/// It was `MenuFrame::overlay: bool`, and that single flag did **two** jobs: it
/// picked the translucent backdrop colour over the opaque one in
/// [`super::draw::build`], *and* it was the only thing suppressing the panorama
/// in `MenuRenderer`'s own `draw`. Any screen wanting a **wash over the panorama** —
/// which is what vanilla's connect and level-loading screens are — could not be
/// expressed: asking for the wash turned the panorama off, so the loading screen
/// rendered as a flat fill with a translucent quad on it and no sky at all.
///
/// # The three states, read off the 26.2 record rather than guessed
///
/// `Screen.extractBackground` is the base implementation, and out of world it is
/// panorama (gated on `minecraft.level == null`) → blur → `menu_background.png`:
///
/// | this enum | vanilla | screens |
/// |---|---|---|
/// | [`Self::Panorama`] | the base `extractBackground` | every out-of-world screen, **including** `ConnectScreen` (no override) and `LevelLoadingScreen`'s `OTHER` reason, whose override calls `extractPanorama` with **no** `level == null` gate |
/// | [`Self::Dim`] | `extractTransparentBackground` / `inworld_menu_background.png` | the pause, death and command-block screens, drawn over a live world |
/// | [`Self::Opaque`] | nothing in vanilla | our fallback when no panorama textures are loaded, e.g. a jar-less or headless run |
///
/// `TitleScreen` is the one screen that wears no wash: its `extractBackground`
/// override is empty and it draws the cubemap itself. That distinction is *not*
/// in this enum — it travels as `MenuFrame::logo` through
/// [`super::panorama::dim_for_screen`], because the wash is applied inside the
/// panorama's own shader rather than as a quad. Do not add a fourth variant for
/// it.
///
/// # The wash is not the backdrop quad
///
/// Worth stating because it is the natural wrong assumption: under
/// [`Self::Panorama`] the full-screen colour quad is **skipped entirely** (see
/// `MenuGeometry::backdrop_floats`), and the 25 %-black
/// `menu_background.png` wash arrives as the panorama shader's own `dim`
/// uniform. So "wash over panorama" needs no quad, and reinstating one would
/// double the darkening.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MenuBackdrop {
    /// The panorama, wearing `menu_background.png`'s wash unless this is the
    /// title screen. Falls back to [`Self::Opaque`]'s flat fill when no panorama
    /// is loaded. The default, because it is what every out-of-world screen
    /// wants.
    #[default]
    Panorama,
    /// A flat opaque fill and nothing else. Only reached as the no-panorama
    /// fallback; no screen asks for it by name.
    Opaque,
    /// A translucent wash over whatever is already on the target — for a frame
    /// drawn with [`MenuRenderer::render_overlay`] over a live, still-rendering
    /// world. Suppresses the panorama, which would cover the world it exists to
    /// leave visible.
    Dim,
    /// [`Self::Dim`]'s vertical-gradient sibling, for the death screen alone.
    /// Vanilla does not reuse `Screen`'s flat `extractTransparentBackground` for
    /// `DeathScreen`: `extractDeathBackground` calls `fillGradient` with two
    /// distinct ARGB endpoints, so the screen reads as
    /// "you died" rather than as a plain pause. Same translucent-over-world
    /// contract as [`Self::Dim`] — it never wants the panorama either — the only
    /// difference is the quad carries two colours instead of one; see
    /// [`super::draw::Quads::rect_vgradient`] and
    /// [`super::DEATH_GRADIENT_TOP`]/[`super::DEATH_GRADIENT_BOTTOM`] for the
    /// decoded constants.
    DeathGradient,
}

impl MenuBackdrop {
    /// Whether [`MenuRenderer`] should prepare and draw the panorama behind this
    /// frame.
    #[must_use]
    pub const fn wants_panorama(self) -> bool {
        matches!(self, Self::Panorama)
    }

    /// Whether the full-screen colour quad should be translucent rather than
    /// opaque. [`Self::Panorama`] answers `false`: its quad is the no-panorama
    /// fallback and must be opaque, and when the panorama *is* up the quad is
    /// skipped and this answer is never used. [`Self::DeathGradient`] answers
    /// `true` for the same reason [`Self::Dim`] does — both endpoints of its
    /// gradient carry alpha well short of 1.0 so the world stays visible
    /// through it — even though [`super::draw::build`] takes a different branch
    /// to emit it (a gradient, not this flat-colour choice).
    #[must_use]
    pub const fn is_translucent(self) -> bool {
        matches!(self, Self::Dim | Self::DeathGradient)
    }
}

/// Everything one menu screen draws.
#[derive(Debug, Clone, Default)]
pub struct MenuFrame<'a> {
    /// Big heading, e.g. `"LODESTONE"`.
    pub title: &'a str,
    /// Small line under the heading.
    pub subtitle: &'a str,
    /// The rows, top to bottom.
    pub rows: Vec<MenuRow>,
    /// Index of the highlighted row. Out-of-range highlights nothing.
    ///
    /// On a screen with a single row cursor this is both "the keyboard is here"
    /// and "the mouse is here", which is why [`draw_widget`] feeds it to
    /// `Widget::focused`. On a screen with real focus
    /// ([`super::Screen::WorldSelect`]) it is the **focused** row only, and
    /// [`Self::hovered`] carries the other fact.
    pub selected: usize,
    /// The row the cursor is over, when that is a different question from
    /// [`Self::selected`].
    ///
    /// `None` on every screen with a row cursor, which is every screen except
    /// [`super::Screen::WorldSelect`] — so nothing about the existing screens'
    /// pixels changes. Vanilla's sprite argument is `isHoveredOrFocused()`
    ///, the `||` of the two, and
    /// [`Widget::is_hovered_or_focused`] is where that join lives; this field is
    /// only how the second operand reaches it. See
    /// [`super::world_select::WorldSelectNav::hovered`] for the bug that made the
    /// split necessary — one flag would let a mouse-over steal the keyboard out
    /// of a text field.
    pub hovered: Option<usize>,
    /// Key-hint lines drawn at the bottom.
    pub footer: Vec<String>,
    /// A message above the footer, drawn in the failure colour.
    pub message: Option<String>,
    /// The user's `gui_scale` option (`0` = auto). [`frame_for`] stamps this
    /// onto every screen's frame, not just [`super::Screen::Settings`]'s — the
    /// whole menu must scale, not only the screen that edits the setting.
    /// Carried on the frame rather than as a new parameter to
    /// [`MenuRenderer::render`] so that call site (owned by `app.rs`) does not
    /// need to change. See [`logical_canvas`].
    pub gui_scale: u32,
    /// The user's `panoramaSpeed` option, `0.0..=1.0` — see
    /// [`crate::config::Options::panorama_speed`]. Stamped onto every screen's
    /// frame by [`frame_for`] for [`Self::gui_scale`]'s reason, and read by
    /// [`MenuRenderer`]'s panorama block, which hands it to
    /// `super::panorama::PanoramaRenderer::set_speed` before advancing the spin.
    ///
    /// **An `Option` rather than a bare `f32`, and that is load-bearing rather
    /// than defensive.** `MenuFrame` derives `Default`, so a bare `f32` would be
    /// `0.0` on any frame built by hand — and `0.0` is a *legitimate* setting
    /// here, a deliberately stationary panorama. An unstamped hermetic frame would
    /// therefore freeze the sky and look exactly like the option working
    /// correctly. `None` means "nothing said", and the renderer leaves its own
    /// speed alone; the same reason [`Self::cursor`] is an `Option` instead of
    /// defaulting to `(0, 0)`.
    pub panorama_speed: Option<f32>,
    /// What sits behind this screen's widgets. See [`MenuBackdrop`] — the three
    /// states this used to try to express with one `overlay: bool`.
    pub backdrop: MenuBackdrop,
    /// Whether [`MenuRenderer::render_overlay`](super::MenuRenderer::render_overlay)
    /// must run the background blur ([`super::blur::MenuBlur`]) before drawing
    /// this frame's widgets.
    ///
    /// **Not implied by `backdrop == MenuBackdrop::Dim`.** Vanilla's own fork
    /// (`Screen::isInGameUi`) is a *third* axis, orthogonal to translucency:
    /// `Screen::extractBackground` blurs for every screen where
    /// `isInGameUi()` is `false` (Pause, in-world Options, Statistics, Social,
    /// Server Links, the in-world resource-pack prompt — vanilla's
    /// `INWORLD_MENU_BACKGROUND` wash, which this client already approximates
    /// with the `Dim` quad) and does **not** for the screens that override it
    /// to `true` (`AbstractContainerScreen` and its sign-edit/command-block-edit
    /// siblings — `extractTransparentBackground`'s flat gradient only, no
    /// blur), even though those also use `Dim` here. So each overlay-frame
    /// builder sets this by hand, the same way each already sets `backdrop`
    /// by hand — see [`super::pause_frame`]/`super::nav::stats_overlay_frame`
    /// and this field's own call sites.
    pub blur: bool,
    /// This frame reproduces one of **vanilla's own** screens: its rows carry
    /// [`MenuRow::slot`]s, its buttons draw as `widget/button*` nine-slice
    /// sprites, and the row-stack's centred title/subtitle/footer block is
    /// suppressed in favour of [`Self::labels`].
    ///
    /// A flag rather than an inference from `rows[0].slot.is_some()`: the two
    /// are different questions (a screen could gain one slotted row), and a
    /// screen silently switching layout mode because of a row edit is exactly
    /// the kind of drift this file's `owns_frame`/`frame_for` agreement test
    /// exists to prevent.
    pub vanilla: bool,
    /// Draw vanilla's `title/minecraft` + `title/edition` logo pair at the top —
    /// the title screen only. A no-op without a GUI atlas carrying those loose
    /// textures (see [`crate::resources::TITLE_TEXTURES`]).
    pub logo: bool,
    /// Blit `BookViewScreen.BOOK_LOCATION`'s top-left 192×192 region behind
    /// this frame. The texture is supplied by the menu atlas, rebuilt from the
    /// active resource-pack stack whenever its generation advances.
    pub book_background: bool,
    /// Free-standing strings at vanilla-derived positions: the pause screen's
    /// "Game Menu" heading, the title screen's version string and copyright
    /// line.
    pub labels: Vec<MenuLabel>,
    /// The mouse position in **logical** pixels, when it is known.
    ///
    /// Every other screen here resolves the mouse to a *row index* before it ever
    /// reaches a frame ([`super::nav::MenuNav::hover`]), which is all a button
    /// needs. The multiplayer list needs more: vanilla's row draws a different
    /// sprite depending on which **quadrant of the 32 px favicon** the cursor is
    /// in, and that cannot be decided before
    /// the canvas is known, because the icon's rect depends on it. So the raw
    /// position rides along on the frame and [`draw_server_entry`] does the
    /// quadrant test against the rect it is about to draw into.
    ///
    /// `None` means "no mouse has moved yet", which is the state a keyboard-only
    /// session and every hermetic test are in — and it must draw *no* hover
    /// overlay rather than one at `(0, 0)`.
    pub cursor: Option<(f32, f32)>,
    /// A wrapped, bounded block of body text — see [`MenuNotice`], which is also
    /// where the overflow bug this exists to fix is described.
    ///
    /// One per frame, because the three states that use it are mutually
    /// exclusive: a sign-in URL, a failure reason, or a save error. Distinct from
    /// [`Self::message`], which is a single unwrapped [`TEXT_SCALE`] line and is
    /// suppressed entirely on a `vanilla` frame.
    pub notice: Option<MenuNotice>,
    /// This screen's scrolling list, if it has one — the generic hook that replaced
    /// [`super::draw`]'s by-name `server_scroll_list` call.
    ///
    /// **Stamped once for every screen** by [`frame_for`] from
    /// [`super::nav::MenuNav::active_list`], not filled in per-arm, for the reason
    /// [`Self::gui_scale`] is: a screen that has a list must not also have to
    /// remember to tell the draw about it. One declaration, two consumers — the
    /// scrollbar in [`super::draw`] and the wheel arm in `app`.
    ///
    /// `None` means "no scrolling list on this screen", which is most of them, and
    /// the draw then paints no bar at all rather than a full-height stub.
    pub list: Option<widget::ListSpec>,
    /// Labels belonging to [`Self::list`]'s rows, drawn **clipped** to the band
    /// that spec declares. Empty for every screen that has no list.
    ///
    /// **The separate vector is the primitive, not a convenience.** A list that
    /// scrolls by pixels will routinely have a row half outside its band, and
    /// the only alternative to clipping it is what every unconverted screen
    /// still does: skip any row that does not *wholly* fit — which is exactly
    /// the snap-to-row behaviour pixel scrolling exists to remove. A screen
    /// whose rows are [`MenuRow`]s gets this from [`super::draw`]'s own
    /// per-row `with_clip`; a screen whose rows are free text (`stats.rs`,
    /// where a stat row is not a control and vanilla only narrates it) had
    /// nowhere to put it until this field.
    ///
    /// Drawn unclipped when [`Self::list`] is `None`, which is what a screen
    /// with no scrollbar wants anyway — so this degrades to [`Self::labels`]
    /// rather than to nothing.
    pub list_labels: Vec<MenuLabel>,
    /// The loading screen's progress bar, `None` on every other screen (issue
    /// #449) — see [`MenuProgress`] and [`loading_frame_with_progress`].
    pub progress: Option<MenuProgress>,
    /// The loading screen's chunk-status grid, `None` on every other screen
    /// and `None` on the loading screen itself until a view radius is known
    /// — see [`ChunkGridView`] and
    /// [`loading_frame_with_progress_and_grid`].
    pub chunk_grid: Option<ChunkGridView>,
    /// A tooltip the *screen* asks for, independent of any row —
    /// [`MenuRow::tooltip`]'s frame-level sibling, drawn by the same painter
    /// and last of all.
    ///
    /// A row tooltip covers every widget-shaped hint, but a screen can have
    /// interactive content that is not a row: a hovered run of a book page is
    /// hit-tested against its own text geometry, not against
    /// [`MenuRow::slot`]. A row request wins when both are present, since a
    /// row is the more specific target.
    ///
    /// Lines are `§`-coded strings, like every other string this pipeline
    /// draws.
    pub tooltip: Option<Vec<String>>,
}

/// Vanilla's `LevelLoadingScreen` progress bar, as a frame primitive.
///
/// Geometry is transcribed from vanilla's own level-loading screen rendering: `PROGRESS_BAR_WIDTH =
/// 200`, two pixels tall, black background, filled to `round(progress * 200)` in
/// green, horizontally centred. `dy` is measured from the screen centre so the
/// bar sits under the phase label the same way vanilla's sits under its text.
///
/// **The fraction is not clamped here.** It arrives already bounded by
/// [`crate::menu::loading::TerrainProgress::fraction`], which is where the
/// "never claim completion" rule lives; a second clamp here would put that rule
/// in two places and let one of them drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MenuProgress {
    /// Fill fraction, `0.0..=1.0`.
    pub fraction: f32,
    /// The bar's top edge, in logical pixels from the screen centre.
    pub dy: f32,
}

/// `LevelLoadingScreen.PROGRESS_BAR_WIDTH`.
pub const PROGRESS_BAR_W: f32 = 200.0;

/// The bar's height. `LevelLoadingScreen` fills `textTop + 12` to `textTop + 14`.
pub const PROGRESS_BAR_H: f32 = 2.0;

/// The unfilled track — `LevelLoadingScreen`'s `0xFF000000`.
pub const PROGRESS_BAR_BG: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// The filled portion — `LevelLoadingScreen`'s `0xFF00FF00`.
pub const PROGRESS_BAR_FG: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

/// Vanilla's `LevelLoadingScreen` chunk-status grid, as a frame
/// primitive — the [`crate::menu::loading::TerrainChunkGrid`] data plus the
/// one thing that is a render decision rather than an observation: where its
/// centre sits.
///
/// See [`crate::menu::loading::ChunkCellStatus`]'s doc for why this draws
/// only two of vanilla's twelve status colours, and [`chunk_cell_colour`] for
/// the colours themselves.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkGridView {
    /// The real per-column data.
    pub grid: crate::menu::loading::TerrainChunkGrid,
    /// The grid's vertical centre, in logical pixels from the screen centre —
    /// same convention as [`MenuProgress::dy`].
    pub dy: f32,
}

/// `LevelLoadingScreen.extractChunksForRendering`'s cell size — `size = 2` at
/// its own call site (`margin` is `0` there too, so cells sit flush).
pub const CHUNK_CELL_SIZE: f32 = 2.0;

/// `ChunkStatus.EMPTY`'s colour, `0x545454` — vanilla's own `COLORS` map, not
/// invented.
pub const CHUNK_CELL_EMPTY: [f32; 4] = [84.0 / 255.0, 84.0 / 255.0, 84.0 / 255.0, 1.0];

/// `ChunkStatus.FULL`'s colour — opaque white, vanilla's own `COLORS` map.
pub const CHUNK_CELL_FULL: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// The colour vanilla's `COLORS` map draws a cell of the given status.
///
/// Only two arms because [`crate::menu::loading::ChunkCellStatus`] only has
/// two variants — see that type's doc for why this client cannot observe the
/// other ten of vanilla's statuses, and does not pretend to.
#[must_use]
pub const fn chunk_cell_colour(status: crate::menu::loading::ChunkCellStatus) -> [f32; 4] {
    match status {
        crate::menu::loading::ChunkCellStatus::Empty => CHUNK_CELL_EMPTY,
        crate::menu::loading::ChunkCellStatus::Full => CHUNK_CELL_FULL,
    }
}

/// The top-left corner of cell `(x, z)`, `0..diameter` each, in a
/// `diameter`-cells-square grid centred at `(center_x, center_y)` — logical
/// pixels, floored the same way [`super::draw::build`]'s bar geometry is.
///
/// Transcribed from `LevelLoadingScreen.extractChunksForRendering` with
/// `margin = 0` (its own call site's value):
///
/// ```text
/// int totalWidth = diameter * width - margin;   // width == size here
/// int xStart = xCenter - totalWidth / 2;
/// int xCellStart = xStart + x * width;
/// ```
///
/// A free function rather than inlined at its one call site so the layout
/// gate can compute the same rect the draw does, instead of restating the
/// arithmetic as a second, driftable copy.
#[must_use]
pub fn chunk_cell_origin(
    center_x: f32,
    center_y: f32,
    diameter: usize,
    x: usize,
    z: usize,
) -> (f32, f32) {
    let total = diameter as f32 * CHUNK_CELL_SIZE;
    let start_x = (center_x - total * 0.5).floor();
    let start_y = (center_y - total * 0.5).floor();
    (
        start_x + x as f32 * CHUNK_CELL_SIZE,
        start_y + z as f32 * CHUNK_CELL_SIZE,
    )
}

/// Decoded favicon mosaics, keyed by the status cache's address key.
///
/// Without this, [`frame_for`] would decode every visible server's PNG **every
/// frame** — 60 zlib inflations per second per row for an image that never
/// changes. The cache is keyed by address rather than by row index so reordering
/// or renaming the list does not invalidate it.
#[derive(Debug, Default)]
pub struct FaviconCache {
    /// `None` means "we tried and it did not decode"; that is cached too, so a
    /// broken icon is not re-decoded forever.
    decoded: std::collections::HashMap<String, Option<FaviconMosaic>>,
}

impl FaviconCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The mosaic for `key`, decoding `png` on first use.
    pub fn get(&mut self, key: &str, png: &[u8]) -> Option<FaviconMosaic> {
        if let Some(hit) = self.decoded.get(key) {
            return hit.clone();
        }
        let m = favicon_mosaic(png);
        self.decoded.insert(key.to_string(), m.clone());
        m
    }

    /// Drops the entry for `key` (its server was deleted or re-addressed).
    pub fn forget(&mut self, key: &str) {
        self.decoded.remove(key);
    }

    /// Number of cached decodes, for tests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.decoded.len()
    }

    /// Whether nothing has been decoded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.decoded.is_empty()
    }
}

/// Whether [`frame_for`] will produce a frame for `screen`, i.e. whether this
/// renderer owns the frame (and, in the app, the keyboard).
///
/// Kept beside `frame_for` with a test asserting the two agree for every screen:
/// a predicate that drifts from the builder gives either a screen drawn twice or
/// one drawn not at all.
///
/// [`Screen::Paused`] is **deliberately excluded**, even though it has its own
/// button rows and keyboard navigation (see [`pause_frame`] and
/// [`super::nav::MenuNav`]'s `key_paused`): this set governs the Clear pass
/// that replaces the whole frame, and the pause menu is drawn as an overlay
/// over the world instead (see [`MenuRenderer::render_overlay`]) — the world
/// keeps rendering (and, on a live server, keeps ticking) behind it. Adding
/// `Screen::Paused` here would stop the world rendering for as long as the
/// game is paused, which is exactly the regression [`super::Screen::Paused`]'s
/// own doc comment warns against.
///
/// [`Screen::Connecting`] is **included**: it is the full-frame
/// loading screen, with a flat dark backdrop and no rows to interact with, and
/// nothing behind it worth rendering — no chunk packets arrive until after
/// login, so the Clear pass that replaces the frame costs nothing. The
/// *post-login* terrain stream is a different screen ([`Screen::Playing`]) and
/// stays on the world path as an overlay in `app::redraw` (see its loading
/// block), for the same reason `Paused`/`Death` do.
///
/// **This function does not have the one exception `frame_for` does.**
/// `Screen::Settings` stays in the set unconditionally, because every caller
/// here is about input routing (mouse/keyboard treated as menu rows) and that
/// is true whether or not a world is loaded behind Options. `frame_for`
/// itself returns `None` for `Screen::Settings` when [`super::UiState::
/// settings_in_world`] — see its arm's own doc — so the "agrees with
/// `frame_for`" test only walks screens reached the way `open_settings`
/// (title) does, not `open_settings_from_pause`; see
/// `frame_for_defers_to_an_overlay_for_in_world_settings` for the case this
/// leaves uncovered by that walk.
#[must_use]
pub fn owns_frame(screen: super::Screen) -> bool {
    use super::Screen;
    matches!(
        screen,
        // The ownership gate: a full-frame screen with two buttons and nothing
        // behind it worth rendering — no world can be loaded while it is up.
        Screen::Ownership
            | Screen::MainMenu
            | Screen::ServerList
            | Screen::ServerEdit
            | Screen::WorldSelect
            | Screen::Settings
            | Screen::Accounts
            | Screen::Connecting
            | Screen::Error
            | Screen::Credits
            | Screen::Social
            | Screen::Statistics
            | Screen::CreateWorld
            // Issue #540. A full-frame screen with two buttons and nothing behind
            // it worth rendering — the world list it was opened from is a menu
            // screen too, so the Clear pass costs nothing here.
            | Screen::Confirm
    )
}
