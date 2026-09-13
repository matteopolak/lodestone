use super::edit_box::EditBox;
use super::focus::{self, FocusChildren, FocusSet, FocusTarget, KeyEvent, KeyOutcome};
use super::model::{MenuKey, MAX_ADDRESS_CHARS};
use super::servers::{MAX_NAME_CHARS, ServerEntry};
use super::sign_edit;

/// Which field of the add/edit form has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    /// The display label.
    Name,
    /// `host` or `host:port`.
    Address,
}

impl Default for FormField {
    fn default() -> Self {
        Self::Name
    }
}

/// [`EditForm`]'s name field, as a [`super::focus::FocusChildren`] id.
///
/// The ids double as the row indices [`super::render::frame_for`] builds and
/// `app.rs`'s hit-test reports, which is why they are `0`/`1` and not opaque —
/// `the_form_field_ids_are_the_row_indices_the_mouse_reports` asserts the two
/// still agree.
pub const NAME_FIELD: usize = 0;
/// [`EditForm`]'s address field. See [`NAME_FIELD`].
pub const ADDRESS_FIELD: usize = 1;

/// Row indices [`crate::menu::render::screens::sign_edit_frame`] builds and
/// this module's own hover/click routing agree on — the [`sign_edit`] screen's
/// version of [`NAME_FIELD`]/[`ADDRESS_FIELD`] above.
pub mod sign_edit_row {
    /// The four line fields, top to bottom.
    pub const LINES: std::ops::Range<usize> = 0..super::sign_edit::LINE_COUNT;
    /// The Done button — the only other row this screen draws.
    pub const DONE: usize = super::sign_edit::LINE_COUNT;
}
/// The server-edit screen's resource-pack cycle button
///. **Live**: a click cycles
/// [`super::servers::ServerPackPolicy`] (`MenuNav::click`'s `ServerEdit`
/// arm), and the value is what a live join now reads to decide whether a
/// pushed resource pack is silently applied, silently declined, or prompted
/// — see `net.rs`'s resource-pack flow. This row used to be present and
/// permanently inactive, on the grounds that `ServerEntry` carried no
/// `pack_status` field to cycle; that gap is closed.
pub const RESOURCE_PACK_ROW: usize = 2;
/// The Done row — saves the
/// form. A real, clickable row alongside the existing Enter/Tab keyboard path
/// (see [`MenuNav::click`]'s `Screen::ServerEdit` arm).
pub const DONE_ROW: usize = 3;
/// The Cancel row — discards
/// the form. See [`DONE_ROW`].
pub const CANCEL_ROW: usize = 4;

/// The logical canvas [`EditForm`]'s boxes are seeded against.
///
/// It matters for exactly two things — the **relative** y order of the two
/// fields (which is what makes Up/Down move between them, since arrow
/// navigation is geometric) and the box **width** scrolls against.
/// `super::render::row_rect` centres the stack vertically, so the ordering holds
/// at every canvas, and it clamps the width to the configured row width at every
/// canvas at least one horizontal padding unit wider — so a seeded box is correct everywhere that is not a
/// pathologically narrow window.
///
/// It is a *seed*, not the draw geometry: `super::render::build` moves a
/// per-frame clone of each box into that frame's real rect (see
/// `super::render`'s `draw_edit_box`), which is the settings sub-screen's
/// reposition-don't-rebuild order. A `&mut MenuNav` per frame would let the
/// originals be repositioned instead, and `frame_for` takes `&MenuNav` — see
/// `docs/menu-focus.md` on why that is `app.rs`'s call to make.
pub(super) const SEED_CANVAS: (f32, f32) = (854.0, 480.0);

/// The two [`EditBox`]es [`EditForm`] owns, as one struct so
/// [`super::focus::FocusSet`] can borrow them while `EditForm` borrows the set.
///
/// **This split is load-bearing, not cosmetic.** `FocusSet`'s methods take
/// `&mut dyn FocusChildren`, and a `FocusSet` living in the same struct as the
/// children it dispatches to could not be called at all — `&mut self.focus` and
/// `&mut self` are not disjoint. Vanilla has the same shape (a `Screen` holds
/// both) and no such rule.
#[derive(Debug, Clone, PartialEq)]
pub struct FormFields {
    /// The display label. Capped at [`MAX_NAME_CHARS`].
    pub name: EditBox,
    /// `host` or `host:port`. Capped at [`MAX_ADDRESS_CHARS`].
    pub address: EditBox,
}

impl FocusChildren for FormFields {
    fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
        match id {
            NAME_FIELD => Some(&self.name as &dyn FocusTarget),
            ADDRESS_FIELD => Some(&self.address as &dyn FocusTarget),
            _ => None,
        }
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
        match id {
            NAME_FIELD => Some(&mut self.name as &mut dyn FocusTarget),
            ADDRESS_FIELD => Some(&mut self.address as &mut dyn FocusTarget),
            _ => None,
        }
    }
}

/// What one key did to the form, from the screen's point of view.
///
/// Only [`Self::Save`] and [`Self::Cancel`] need the screen's cooperation; every
/// other keystroke was fully answered by the focused [`EditBox`] or by focus
/// navigation. This is the distinction [`super::focus::KeyOutcome`] exists to
/// preserve and vanilla's `boolean` throws away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormOutcome {
    /// The field or the focus layer dealt with it.
    Handled,
    /// Escape: discard the form.
    Cancel,
    /// Enter, and nothing else wanted it: save.
    Save,
}

/// The add/edit form's contents: two real [`EditBox`]es and the focus that
/// decides which of them a keystroke reaches.
///
/// The address is held as the **single string the user typed** and split into
/// host/port only on save. Splitting per keystroke would make `mc.example.com:2`
/// unrepresentable halfway through typing `:25565`.
///
/// ## These widgets outlive a frame, and that is the point
///
/// Every other screen in this shell rebuilds its rows — labels included — in
/// [`super::render::frame_for`], every frame. That is fine for a button, whose
/// whole state is derivable, and impossible for a text field: rebuilding one
/// would reset the caret, the selection and the scroll offset sixty times a
/// second. Rebuilding the screen has exactly this consequence in the reference
/// client too — it clears focus, so a rebuilt screen has no
/// focus by construction.
///
/// This is menu widget state rather than derived state. The layout's
/// build→reposition order keeps each box's persistent state separate from the
/// per-frame geometry; [`super::render`]'s `draw_edit_box` receives a clone so
/// rendering cannot mutate the navigation state in place.
#[derive(Debug, Clone, PartialEq)]
pub struct EditForm {
    /// The two fields. Public so [`super::render`] can read a box's own
    /// geometry, value, caret and selection rather than re-deriving them.
    pub fields: FormFields,
    /// Which field has focus, and the Tab/arrow traversal between them.
    pub(super) focus: FocusSet,
    /// Index being edited, or `None` when adding a new entry.
    pub editing: Option<usize>,
    /// Which of [`RESOURCE_PACK_ROW`]/[`DONE_ROW`]/[`CANCEL_ROW`] the mouse is
    /// over, if any — separate from [`Self::field`] the same way
    /// `WorldSelectNav::hovered` is separate from its own focus, and for the
    /// same reason: those three rows are buttons, not text fields, so a mouse
    /// hovering one must not steal keyboard focus out of whichever field it
    /// was in. See [`super::render::MenuFrame::hovered`].
    hovered: Option<usize>,
    /// The server-edit screen's resource-pack cycle-control value — see
    /// [`super::servers::ServerPackPolicy`]. Seeded from the entry being
    /// edited, or [`super::servers::ServerPackPolicy::default`] (`Prompt`)
    /// for a new one, and carried into [`Self::to_entry`].
    pack_status: super::servers::ServerPackPolicy,
}

impl Default for EditForm {
    fn default() -> Self {
        Self::adding()
    }
}

impl EditForm {
    /// A blank form for a new entry, focused on the name field.
    ///
    /// The initial focus is set explicitly, which is
    /// vanilla's own screen base's set-initial-focus call taking an explicit target rather
    /// than the no-argument overload — that one is gated on
    /// last-input-device state, which this shell does not track. Without an
    /// explicit initial focus the form would open with **nothing** focused
    /// and the first keystroke would go nowhere, which is precisely the island
    /// this issue is about.
    #[must_use]
    pub fn adding() -> Self {
        let [name_rect, address_rect] =
            super::render::field_row_rects(SEED_CANVAS.0, SEED_CANVAS.1);
        // The narration text was "Name"/"Address" — plausible-looking and
        // wrong. The reference labels are
        // "Server Name"/"Server Address", whose English language-table values are
        // "Server Name"/"Server Address" — which happen to already be what
        // `render.rs`'s (unrelated) `detail` line under each field shows, so
        // this was invisible on screen and only wrong to a screen reader.
        let mut name =
            EditBox::new(name_rect.0, name_rect.1, name_rect.2, name_rect.3, "Server Name")
                .with_max_length(MAX_NAME_CHARS);
        // Vanilla shows a default hint name here while the field is empty and
        // unfocused — this is Lodestone's own hint text rather than the
        // reference client's default server name, since the
        // field labels a server the user is adding, not naming our product.
        name.hint = Some("My Server".to_string());
        let mut fields = FormFields {
            name,
            address: EditBox::new(
                address_rect.0,
                address_rect.1,
                address_rect.2,
                address_rect.3,
                "Server Address",
            )
            .with_max_length(MAX_ADDRESS_CHARS),
        };
        let mut focus = FocusSet::new();
        // Register both fields as drawn and interactive controls, rather than
        // registering them only for layout or only for input: these
        // are drawn *and* interactive. Getting this wrong is the
        // island `super::focus`'s docs describe — a field that renders and never
        // takes a keystroke, with nothing failing loudly.
        focus.add_renderable_widget(NAME_FIELD);
        focus.add_renderable_widget(ADDRESS_FIELD);
        focus.set_initial_focus(&mut fields, NAME_FIELD);
        Self {
            fields,
            focus,
            editing: None,
            hovered: None,
            pack_status: super::servers::ServerPackPolicy::default(),
        }
    }

    /// A form pre-filled from `entry`, editing the row at `index`.
    #[must_use]
    pub fn editing(index: usize, entry: &ServerEntry) -> Self {
        let mut form = Self::adding();
        form.fields.name.set_value(&entry.name);
        form.fields.address.set_value(entry.address_label());
        form.editing = Some(index);
        form.pack_status = entry.pack_status;
        form
    }

    /// The name field's text.
    #[must_use]
    pub fn name(&self) -> &str {
        self.fields.name.value()
    }

    /// The address field's text.
    #[must_use]
    pub fn address(&self) -> &str {
        self.fields.address.value()
    }

    /// Which field has focus, for [`super::render`]'s `selected` row.
    ///
    /// Derived from [`super::focus::FocusSet`] rather than stored: two sources of
    /// truth for "which field is being typed into" is how a caret ends up drawn
    /// on a row that is not receiving the keys.
    #[must_use]
    pub fn field(&self) -> FormField {
        match self.focus.focused() {
            Some(ADDRESS_FIELD) => FormField::Address,
            _ => FormField::Name,
        }
}
    /// Whether the form can be saved. The label may be blank (it falls back to
    /// the host); the address may not.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.address().trim().is_empty()
    }

    /// The entry this form would save.
    #[must_use]
    pub fn to_entry(&self) -> ServerEntry {
        let (host, port) = ServerEntry::split_host_port(self.address());
        let name = if self.name().trim().is_empty() {
            host.clone()
        } else {
            self.name().to_owned()
        };
        let mut entry = ServerEntry::new(name, host, port);
        entry.pack_status = self.pack_status;
        entry
    }

    /// The resource-pack cycle control's current value, for the row's label.
    #[must_use]
    pub fn pack_status(&self) -> super::servers::ServerPackPolicy {
        self.pack_status
    }

    /// Advances [`Self::pack_status`] — the `RESOURCE_PACK_ROW` click.
    pub fn cycle_pack_status(&mut self) {
        self.pack_status = self.pack_status.cycle();
    }

    /// One key, routed through the screen key-handling order: Escape, then
    /// the focused field, then — only if it declined — Tab and the arrows as
    /// focus navigation, and only then the screen's own meaning for the key.
    ///
    /// **That order is why Up/Down move between fields while Left/Right move the
    /// caret**, with no rule anywhere saying so: the text-field key handler handles
    /// 262/263 and declines 264/265, so the vertical
    /// pair falls through to navigation and the horizontal pair never gets there.
    pub fn handle_key(&mut self, key: MenuKey) -> FormOutcome {
        // A printable character is handled by a *different* callback in the reference client
        // — see `super::focus::KeyEvent::from_menu_key`. Routing it through
        // routing it through the key handler would make the letter `a` and Ctrl+A the same event.
        if let MenuKey::Char(ch) = key {
            self.focus.char_typed(&mut self.fields, ch);
            return FormOutcome::Handled;
        }
        let Some(event) = KeyEvent::from_menu_key(key) else {
            return FormOutcome::Handled;
        };
        match self.focus.screen_key_pressed(&mut self.fields, event) {
            KeyOutcome::Close => FormOutcome::Cancel,
            KeyOutcome::Consumed | KeyOutcome::FocusMoved => FormOutcome::Handled,
            KeyOutcome::Declined if key == MenuKey::Enter => FormOutcome::Save,
            KeyOutcome::Declined => FormOutcome::Handled,
        }
    }

    /// Focus the field drawn at row `row`, as a mouse click or hover does.
    /// Out-of-range rows are ignored rather than clamped.
    pub fn focus_row(&mut self, row: usize) {
        if row == NAME_FIELD || row == ADDRESS_FIELD {
            self.focus.set_focused(&mut self.fields, Some(row));
        }
    }

    /// The mouse moved over row `row` (issue: the screen's framework
    /// conversion added three button rows this form has no `FocusTarget` for).
    ///
    /// **A field row does nothing here** — a player report (2026-08-04)
    /// caught that pure mouse motion over the name/address field was granting
    /// it real keyboard focus, with no click involved. The input handler
    /// only moves focus from a click or Tab
    /// traversal; hovering a field is not one of those, and `EditBox` does
    /// not even highlight on hover (see [`super::widget::Widget::slider`]'s
    /// sibling asymmetry note in `edit_box.rs`). A button row
    /// ([`RESOURCE_PACK_ROW`]/[`DONE_ROW`]/[`CANCEL_ROW`]) still records
    /// *only* [`Self::hovered`] — that part was always right, since it never
    /// touched keyboard focus — which is what lets the mouse travel to Done
    /// without pulling the caret out of the address field the player is still
    /// typing into. A click (`MenuNav::click`'s `ServerEdit` arm) still calls
    /// [`Self::focus_row`] directly, unaffected.
    pub fn hover_row(&mut self, row: usize) {
        match row {
            NAME_FIELD | ADDRESS_FIELD => {}
            RESOURCE_PACK_ROW | DONE_ROW | CANCEL_ROW => self.hovered = Some(row),
            _ => {}
        }
    }

    /// The button row the mouse is over, for [`super::render::MenuFrame::hovered`].
    #[must_use]
    pub fn hovered_button(&self) -> Option<usize> {
        self.hovered
    }

    /// A click at logical `(x, y)`, dispatched through the input handler — so it
    /// both focuses the field it
    /// landed in and puts the caret at the clicked character.
    ///
    /// Not reachable from `app.rs` today, which routes a click as a *row index*
    /// (`MenuNav::click`) and therefore carries no x. See `docs/menu-focus.md`.
    pub fn click_at(&mut self, x: f32, y: f32) -> bool {
        self.focus.mouse_clicked(&mut self.fields, x, y)
    }

    /// Types one character into the focused field, refusing whatever
    /// [`super::edit_box::is_allowed_chat_character`] refuses.
    ///
    /// Kept as a named method because it is the form's whole text-entry API to
    /// its tests; the cap and the filter now live in [`EditBox`] rather than
    /// here, so `§` is still refused for vanilla's own reason (it is the legacy
    /// formatting-code introducer) rather than for one written twice.
    pub fn push(&mut self, ch: char) {
        self.focus.char_typed(&mut self.fields, ch);
    }

    /// Deletes the character before the caret in the focused field.
    ///
    /// Now genuinely "before the caret" rather than "off the end", which is what
    /// the pre-`EditBox` form did — it had no caret to be before.
    pub fn backspace(&mut self) {
        self.focus
            .key_pressed(&mut self.fields, KeyEvent::new(focus::KEY_BACKSPACE));
    }

    /// Moves focus to the other field, through real Tab traversal.
    ///
    /// With two children this is also the wrap, and the wrap is vanilla's
    /// clear-focus-then-retry rather than modular arithmetic — see
    /// [`super::focus`].
    pub fn next_field(&mut self) {
        self.focus
            .screen_key_pressed(&mut self.fields, KeyEvent::new(focus::KEY_TAB));
    }
}
