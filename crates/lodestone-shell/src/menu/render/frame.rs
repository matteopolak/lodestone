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
    /// `copyrightX = width - font.width(text) - 2` (`TitleScreen.java:110-111`)
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
    /// withScale(2.0F))` before drawing it (`DeathScreen.java:23,119`).
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
    /// downloaded skin later (issue #62) a data change, not a rewrite.
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
    /// instead of a `Button` (issue #55).
    ///
    /// A settings screen's numeric options are sliders and its enums and
    /// booleans are `CycleButton`s (`OptionInstance.java:127-135`), and the two
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
    /// `AbstractSliderButton.value` (`AbstractSliderButton.java:28,69-77`) —
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
    /// (`SpriteIconButton.java:236-244`). `label` is still carried (it is the
    /// tooltip/narration text in vanilla) but not drawn.
    pub icon: Option<&'static str>,
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
}

/// One account-list row's state (issues #66/#402).
///
/// Deliberately two fields. Everything else a row draws is already a [`MenuRow`]
/// field — the username is `label`, "Microsoft account" is `detail`, the
/// "Selected" marker is `trailing`, the head icon is `head` — and duplicating any
/// of them here is how a row and its draw end up disagreeing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountEntryView {
    /// The row's index **in the rendered window**, not in the full account list:
    /// the frame builder has already applied the scroll offset, so this is what
    /// [`accounts_row_top`] multiplies and what a click hit-tests onto.
    pub index: usize,
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
    pub text: String,
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
    /// (`AbstractButton.java:43-53`), the `||` of the two, and
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
    /// Whether this frame is drawn **over** an already-rendered scene rather
    /// than replacing it — [`Screen::Paused`](super::Screen::Paused)'s pause
    /// menu, via [`pause_frame`] and
    /// [`MenuRenderer::render_overlay`]. Changes only how [`geometry`] paints
    /// the full-screen backdrop (translucent instead of opaque, so the world
    /// stays visible behind the buttons); every other screen leaves this
    /// `false` via `..Default::default()`.
    pub overlay: bool,
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
    /// in (`ServerSelectionList.java:364-395`), and that cannot be decided before
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
        Screen::MainMenu
            | Screen::ServerList
            | Screen::ServerEdit
            | Screen::WorldSelect
            | Screen::Settings
            | Screen::Accounts
            | Screen::Error
            | Screen::Credits
            | Screen::Social
            | Screen::Statistics
            | Screen::CreateWorld
    )
}

