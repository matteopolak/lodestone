use super::*


/// **Every avatar on the accounts screen was the same hand-authored head.**
///
/// The discriminating assertion is that the two rows differ *from each other*:
/// asserting only "row 0 has a head" passes under the old behaviour, because it
/// always did — `head` was `Some(default_head_icon())` unconditionally, so the
/// field was never empty and never right. Each row is additionally pinned to
/// *its own* sheet's face colour, so two accounts cannot both be resolved
/// through whichever skin happened to be fetched first.
///
/// The sheets are installed through `remote_skins::publish`, the seam that
/// exists so a headless gate never performs an HTTP GET as a side effect of
/// `cargo test`.
#[test]
fn each_account_row_draws_its_own_skins_face_not_one_shared_head() {
    const URL_A: &str = "https://textures.minecraft.net/texture/aaaaaaaa";
    const URL_B: &str = "https://textures.minecraft.net/texture/bbbbbbbb";
    // Fully transparent hats, so each row's face colour is exactly its own and
    // the hat's contribution is asserted separately below.
    crate::remote_skins::publish(URL_A.to_owned(), skin_sheet([0xFF, 0x00, 0x00], 0, [0, 0, 0]));
    crate::remote_skins::publish(URL_B.to_owned(), skin_sheet([0x00, 0x00, 0xFF], 0, [0, 0, 0]));

    let path = std::env::temp_dir().join(format!(
        "lodestone-render-accounts-{}-per-account-head/servers.json",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let mut meta = lodestone_auth::metadata::AccountsMetadata::default();
    for (i, url) in [URL_A, URL_B].iter().enumerate() {
        meta.upsert(lodestone_auth::metadata::AccountProfile {
            profile_id: uuid::Uuid::from_u128(i as u128 + 1),
            username: format!("p{i}"),
            skin_url: Some((*url).to_string()),
            last_used: (2 - i) as u64,
        });
    }
    meta.save_to(&path.parent().unwrap().join("profiles.json"))
        .expect("the temp profiles file must be writable");
    let nav = MenuNav::with_path(path.clone());

    let f = accounts_idle_frame(nav.accounts());
    let head = |row: usize| -> super::FaviconMosaic {
        f.rows[row]
            .head
            .clone()
            .unwrap_or_else(|| panic!("row {row} must carry a head icon"))
    };
    let (a, b) = (head(0), head(1));
    let placeholder = super::default_head_icon();

    assert_ne!(
        a, placeholder,
        "row 0 still draws the hand-authored placeholder -- its skin url never \
         reached the frame"
    );
    assert_ne!(b, placeholder, "row 1 still draws the hand-authored placeholder");
    assert_ne!(
        a, b,
        "both rows resolved to the same avatar -- the head is not per-account"
    );
    assert_eq!(
        a,
        super::favicon::face_mosaic(&skin_sheet([0xFF, 0x00, 0x00], 0, [0, 0, 0]))
            .expect("a 64x64 sheet has a face"),
        "row 0 must draw its OWN sheet's face"
    );
    assert_eq!(
        b,
        super::favicon::face_mosaic(&skin_sheet([0x00, 0x00, 0xFF], 0, [0, 0, 0]))
            .expect("a 64x64 sheet has a face"),
        "row 1 must draw its OWN sheet's face"
    );
    // The offline row has no account and no stored url, so it keeps the
    // placeholder -- the control proving the assertions above are not passing
    // because every head changed.
    assert_eq!(
        head(2),
        placeholder,
        "the offline row is not a Microsoft account and has no skin url"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// The hat layer is half of a Minecraft face: vanilla's `PlayerFaceRenderer`
/// blits `(8, 8)` and then `(40, 8)` **over** it, so a skin whose character is
/// its helmet or hair is unrecognisable from the base layer alone.
///
/// An opaque hat must therefore win outright, and a fully transparent one must
/// change nothing — two inputs that give different answers only if the
/// composite actually runs. A gate with one arm cannot tell "the hat is
/// composited" from "the hat rect is being read as the face".
#[test]
fn the_face_mosaic_composites_the_hat_layer_over_the_base() {
    let bare = super::favicon::face_mosaic(&skin_sheet([0xFF, 0x00, 0x00], 0, [0x00, 0x00, 0xFF]))
        .expect("a 64x64 sheet has a face");
    let hatted = super::favicon::face_mosaic(&skin_sheet([0xFF, 0x00, 0x00], 255, [0x00, 0x00, 0xFF]))
        .expect("a 64x64 sheet has a face");
    assert_ne!(
        bare, hatted,
        "an opaque hat layer changed nothing -- it is not being composited"
    );
    // And an opaque hat is *exactly* a face of the hat's own colour, since both
    // rects are flat here.
    let hat_only = super::favicon::face_mosaic(&skin_sheet([0x00, 0x00, 0xFF], 0, [0, 0, 0]))
        .expect("a 64x64 sheet has a face");
    assert_eq!(
        hatted, hat_only,
        "an opaque hat must fully cover the base face"
    );
    // A sheet too small to hold the hat rect declines rather than reading past
    // the end of the buffer.
    let tiny = lodestone_assets::Image {
        width: 8,
        height: 8,
        rgba: vec![0u8; 8 * 8 * 4],
    };
    assert!(super::favicon::face_mosaic(&tiny).is_none(), "an 8x8 sheet has no face rect");
}

#[test]
fn the_accounts_slots_do_not_depend_on_the_reference_canvas() {
    // The same argument the multiplayer screen's version of this makes: the
    // block is arranged **once** at `ACCOUNTS_REF_CANVAS`, which is sound only
    // if every rect it hands out is canvas-independent once expressed as a
    // `Slot`. Even widths only — `Origin::ScreenBottom`'s x is `width * 0.5`
    // unrounded while `FrameLayout` truncates, so an odd logical width differs
    // by half a pixel (the limit `Screen::WorldSelect`'s footer has too).
    for (w, h) in [(854.0, 480.0), (1280.0, 720.0), (640.0, 400.0)] {
        let live = AccountsBlock::at(w, h);
        assert_eq!(
            accounts_block().content_top,
            live.content_top,
            "the content band moved at {w}x{h}"
        );
        for i in 0..crate::menu::accounts::BUTTON_COUNT {
            let slot = accounts_button_slot(i);
            assert_eq!(
                slot,
                live.footer_slot(i),
                "button {i}'s slot depends on the canvas"
            );
            // ...and it must resolve onto *that* canvas' own arrangement,
            // which is what makes the two derivations independent rather than
            // merely equal.
            let got = slot.resolve(w, h);
            let want = live.footer[i];
            assert!(
                (got.0 - want.0).abs() < 0.01
                    && (got.1 - want.1).abs() < 0.01
                    && (got.2 - want.2).abs() < 0.01
                    && (got.3 - want.3).abs() < 0.01,
                "button {i} resolves to {got:?} at {w}x{h}, arranged at {want:?}"
            );
        }
    }
    // The footer column measures `4 * 74 + 3 * 4`, which is the multiplayer
    // screen's lower row exactly — the agreement `ACCOUNTS_BUTTON_W`'s doc
    // claims, asserted rather than described.
    let first = accounts_button_slot(0).resolve(854.0, 480.0);
    let last = accounts_button_slot(crate::menu::accounts::BUTTON_COUNT - 1)
        .resolve(854.0, 480.0);
    let column = last.0 + last.2 - first.0;
    let want = 4.0 * ACCOUNTS_BUTTON_W + 3.0 * ACCOUNTS_FOOTER_SPACING as f32;
    assert!(
        (column - want).abs() < 0.01,
        "the footer column is {column}, not {want}"
    );
}

#[test]
fn the_account_rows_are_in_the_order_click_assumes() {
    // `AccountsNav::hover` maps a **rendered** row index back through the
    // scroll window and then onto the four button slots, so this order is a
    // coupling between two files — the same guard shape the settings and
    // multiplayer screens carry against the same row-index mismatch.
    use crate::menu::accounts::{
        BUTTON_ADD, BUTTON_CANCEL, BUTTON_COUNT, BUTTON_REMOVE, BUTTON_SELECT,
    };
    let nav = accounts_nav("order", &["Alex", "Steve"]);
    let f = accounts_idle_frame(nav.accounts());

    assert_eq!(
        f.rows.len(),
        3 + BUTTON_COUNT,
        "two accounts + the offline entry + four buttons"
    );
    for (i, row) in f.rows.iter().take(3).enumerate() {
        let view = row
            .account
            .as_ref()
            .unwrap_or_else(|| panic!("row {i} is not a list row"));
        assert_eq!(view.index, i, "row {i} carries the wrong rendered index");
    }
    for (button, label) in [
        (BUTTON_ADD, "Add Account"),
        (BUTTON_SELECT, "Select"),
        (BUTTON_REMOVE, "Remove"),
        (BUTTON_CANCEL, "Back"),
    ] {
        let row = &f.rows[3 + button];
        assert_eq!(row.label, label, "button {button} is labelled wrong");
        assert_eq!(
            row.slot,
            Some(accounts_button_slot(button)),
            "{label} is not in its own footer slot"
        );
        assert!(row.account.is_none(), "{label} must not be a list row");
    }

    // The two cursors are separate: the keyboard starts on row 0, which is the
    // *list* cursor, and no footer button may be lit while it is there.
    assert!(f.rows[0].account.as_ref().unwrap().selected);
    assert_eq!(
        f.selected,
        usize::MAX,
        "a button is highlighted while focus is on a row"
    );
}

/// **The island closed: the offline row's label is the persisted name.**
///
/// `crate::offline_identity` shipped the model, the file and the derived UUID with
/// no reader — this frame hardcoded `"Play offline"`, so the one name every join
/// in this client uses reached zero pixels. Asserted through the real
/// `accounts_idle_frame`, not through `AccountsNav::offline_username`, because the
/// accessor being right is exactly what the bug was compatible with.
#[test]
fn the_offline_row_carries_the_persisted_name_through_the_real_frame() {
    // Called for its side effect: it wipes the temp root and writes a
    // `profiles.json` there. Dropped immediately — the nav under test is the
    // second one, built *after* the identity file exists, because `with_path`
    // reads the identity once at construction.
    drop(accounts_nav("offline-label", &["Alex"]));
    // Written to the *same* temp root the nav derives its own offline path from,
    // so this proves the derivation as well as the label. `MenuNav::with_path`
    // takes `servers.json`; `AccountsNav::with_path` takes `profiles.json`
    // beside it; the identity is `offline.json` beside that.
    let dir = std::env::temp_dir().join(format!(
        "lodestone-render-accounts-{}-offline-label",
        std::process::id()
    ));
    let mut id = crate::offline_identity::OfflineIdentity::default();
    id.set_username("Notch").expect("valid fixture name");
    id.save_to(&dir.join("offline.json")).expect("temp offline.json");
    let nav = MenuNav::with_path(dir.join("servers.json"));

    let f = accounts_idle_frame(nav.accounts());
    let offline = f
        .rows
        .iter()
        .find(|r| r.detail == "No sign-in required")
        .expect("the offline row must exist");
    assert_eq!(offline.label, "Notch", "the offline row is not showing the persisted name");
    assert_ne!(
        offline.label, "Play offline",
        "the pre-fix literal is still what reaches the frame"
    );

    // The control: a root with **no** `offline.json` shows the default, so the
    // assertion above is measuring the file rather than any constant.
    let fresh = accounts_nav("offline-label-default", &["Alex"]);
    let g = accounts_idle_frame(fresh.accounts());
    let fresh_row = g
        .rows
        .iter()
        .find(|r| r.detail == "No sign-in required")
        .expect("the offline row must exist");
    assert_eq!(
        fresh_row.label,
        crate::offline_identity::DEFAULT_USERNAME,
        "with no offline.json the row must show the default name"
    );
    assert_ne!(
        fresh_row.label, offline.label,
        "both roots produced the same label, so neither is reading the file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The island gate for the name editor: `frame_for` really reaches it.**
///
/// `accounts_name_edit_frame` and every one of `AccountsNav`'s editor tests form
/// a closed loop — they can all be green while `dispatch.rs`'s `Screen::Accounts`
/// arm still only ever consults `sign_in_view()`, in which case pressing "Edit
/// Name" swallows the keyboard and draws the *list* frame underneath it. Nothing
/// but a gate through `frame_for` — the function `app.rs` calls every frame — can
/// see that, so this drives that function and nothing else.
///
/// It also checks the two things an early `return` out of `frame_for` would have
/// silently dropped: `gui_scale` and `list` are stamped after the `match`, so an
/// editor frame produced by a `return` would ignore the GUI-scale setting.
#[test]
fn frame_for_reaches_the_name_editor_and_still_stamps_the_frame() {
    let nav = accounts_nav("editor-dispatch", &[]);
    // `unavailable_probe`, like every other `frame_for` test here: a real prober
    // would reach the network from a unit test.
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::default();
    let mut ui = UiState::new();
    ui.open_accounts();

    // Before: the list frame, with the offline row on it.
    let before = frame_for(&ui, &nav, &statuses, &mut fav).expect("the list frame");
    assert!(
        before.rows.iter().any(|r| r.detail == "No sign-in required"),
        "precondition: the list frame must be what shows before the editor opens"
    );

    let accounts = nav.accounts();
    accounts.hover(accounts.rows().len() + crate::menu::accounts::BUTTON_REMOVE);
    accounts.handle_key(MenuKey::Enter);
    assert!(
        accounts.is_editing_name(),
        "precondition: the editor must be open, or 'the frame changed' is vacuous"
    );

    let after = frame_for(&ui, &nav, &statuses, &mut fav).expect("the editor frame");
    assert!(
        after.rows.iter().any(|r| r.edit.is_some()),
        "frame_for did not reach the editor: no row carries an EditBox, so the \
         screen still draws the account list while the keyboard types into an \
         invisible field. Rows were {:?}",
        after.rows.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    assert!(
        !after.rows.iter().any(|r| r.detail == "No sign-in required"),
        "the list rows are still being drawn, so this is the list frame"
    );
    // Stamped, and asserted against `nav`'s own value rather than a literal —
    // the same source `frame_for`'s tail reads.
    assert_eq!(after.gui_scale, nav.gui_scale(), "gui_scale was not stamped");
    // And it really draws: measured through the same `geometry` the screen sweep
    // uses, at a canvas the whole frame is on.
    assert!(
        !geometry(&after, 1280.0, 720.0).is_empty(),
        "the editor frame draws nothing"
    );
}

/// The name editor's frame: a real [`EditBox`] row plus a Done button, with the
/// UUID of the **typed** name on screen.
///
/// The `edit.is_some()` assertion is the island guard `create_world::frame`'s own
/// doc explains at length: a focusable, typeable box that no row carries draws
/// nothing at all, and every unit test of the box itself still passes.
#[test]
fn the_name_editor_frame_carries_a_real_edit_box_and_the_typed_uuid() {
    use crate::menu::accounts::{NAME_EDIT_DONE_ROW, NAME_EDIT_FIELD_ROW};

    let nav = accounts_nav("name-editor", &[]);
    let accounts = nav.accounts();
    // Open it through the production path: the offline row is row 0 with no
    // accounts, and the third footer button is the affordance.
    accounts.hover(accounts.rows().len() + crate::menu::accounts::BUTTON_REMOVE);
    accounts.handle_key(MenuKey::Enter);
    for _ in 0..20 {
        accounts.handle_key(MenuKey::Backspace);
    }
    for ch in "Notch".chars() {
        accounts.handle_key(MenuKey::Char(ch));
    }

    let view = accounts.name_edit_view().expect("the editor must be open");
    let f = super::account_screen::accounts_name_edit_frame(&view);
    assert_eq!(f.rows.len(), 2, "the editor is a field and a Done button");
    let field = &f.rows[NAME_EDIT_FIELD_ROW];
    assert!(
        field.edit.is_some(),
        "the field row carries no EditBox, so nothing draws the caret, the \
         selection or the visible slice"
    );
    assert!(field.field, "the field row is not drawn as a field");
    assert_eq!(field.edit.as_ref().unwrap().value(), "Notch");
    assert_eq!(f.rows[NAME_EDIT_DONE_ROW].label, "Done");
    assert!(
        f.rows[NAME_EDIT_DONE_ROW].edit.is_none(),
        "a button row must not carry an EditBox"
    );
    assert_eq!(
        f.selected, NAME_EDIT_DONE_ROW,
        "the row cursor must be on Done, not behind the text field"
    );

    // The UUID line, and its expected value comes from `offline_identity`'s
    // externally-computed vector rather than from `offline_uuid` re-derived here.
    const NOTCH: &str = "b50ad385-829d-3141-a216-7e7d7539ba7f";
    assert!(
        f.labels.iter().any(|l| l.text.contains(NOTCH)),
        "the typed name's UUID is not on screen; labels were {:?}",
        f.labels.iter().map(|l| &l.text).collect::<Vec<_>>()
    );
    // Control: the *default* name's UUID must not also be there, or the label
    // could be showing the saved identity and this would still pass.
    let player = crate::offline_identity::offline_uuid("Player").to_string();
    assert!(
        !f.labels.iter().any(|l| l.text.contains(&player)),
        "the saved name's UUID is on screen too, so the line is not following \
         what was typed"
    );

    // Both rows have a `slot`, and the two must not be the same rect — a field
    // drawn on top of its own Done button is a screen with one visible widget.
    let field_slot = field.slot.expect("the field must have a slot");
    let done_slot = f.rows[NAME_EDIT_DONE_ROW]
        .slot
        .expect("Done must have a slot");
    assert_ne!(field_slot, done_slot);
    let (fx, fy, fw, fh) = field_slot.resolve(854.0, 480.0);
    let (dx, dy, dw, dh) = done_slot.resolve(854.0, 480.0);
    assert!(
        fy + fh <= dy || dy + dh <= fy,
        "the field ({fx},{fy},{fw},{fh}) and Done ({dx},{dy},{dw},{dh}) overlap \
         vertically"
    );
    // Both on-canvas, measured rather than assumed: "nothing drew" is free for a
    // rect that is off the canvas entirely.
    for (name, (x, y, w, h)) in [("field", (fx, fy, fw, fh)), ("done", (dx, dy, dw, dh))] {
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= 854.0 && y + h <= 480.0,
            "{name} resolves off-canvas at ({x},{y},{w},{h})"
        );
    }
}

#[test]
fn an_account_row_draws_inside_its_own_36px_row_and_not_the_one_below() {
    let nav = accounts_nav("rowpixels", &["Alex"]);
    let f = accounts_idle_frame(nav.accounts());
    let (w, h) = (854.0, 480.0);
    let v = geometry(&f, w, h);

    // Row 0 is Alex, row 1 the offline entry, row 2 is past the end.
    for i in 0..2 {
        let rect = accounts_row_rect(i, w, 0.0);
        assert!(
            coverage(&v, w, h, rect) > 0.05,
            "row {i} drew nothing in {rect:?}: {}",
            coverage(&v, w, h, rect)
        );
    }
    let empty = accounts_row_rect(2, w, 0.0);
    assert_eq!(
        coverage(&v, w, h, empty),
        0.0,
        "something drew in the row past the end, at {empty:?}"
    );

    // The 32 px head fills the content box's full height, which is the whole
    // point of a 36 px pitch with 2 px of padding.
    let (cx, cy, _, _) = accounts_row_content_rect(0, w, 0.0);
    let head = (cx, cy, ACCOUNTS_HEAD_ICON, ACCOUNTS_HEAD_ICON);
    assert!(
        coverage(&v, w, h, head) > 0.95,
        "the head icon does not fill {head:?}: {}",
        coverage(&v, w, h, head)
    );
}

/// **The reported bug.** The sign-in failure reason was drawn as one
/// unwrapped centred line at [`TEXT_SCALE`], so a message assembled from a
/// server's own response body was both too large to read and wider than the
/// screen.
///
/// Measured by location, against the rect the *draw* derives — `notice_rect`
/// is called here rather than restated, because `CLAUDE.md` records two gates
/// whose restated rect was itself the thing that was wrong — and the failure
/// output is a bounding box, not a fraction. The control is **executed**: the
/// same detector, on the same frame, with a deliberately unbounded wrap
/// column, must report a box outside the rect. Without it, "nothing
/// overflowed" would pass just as well on a frame where nothing drew at all.
#[test]
fn a_long_sign_in_failure_is_wrapped_and_bounded_to_the_notice_rect() {
    // `lodestone-auth`'s `step_result` formats `"{status}: {snippet}"` with up
    // to 400 characters of whatever the server actually returned, and a JSON
    // body has **no whitespace in it** — so a wrap that only breaks on spaces
    // emits one enormous line, and this passes only because `wrap_bounded`
    // breaks mid-word.
    let body = format!(
        "401:{{\"XErr\":2148916238,\"Message\":\"{}\"}}",
        "x".repeat(360)
    );
    assert!(
        !body.contains(' '),
        "premise: the message has no whitespace to wrap on"
    );

    let (w, h) = (854.0, 480.0);
    let frame = accounts_failed_frame(&body);
    let notice = frame
        .notice
        .clone()
        .expect("the failure state must carry a notice");
    let (nx, ny, nw, nh) = notice_rect(&notice, w, h);
    let v = geometry(&frame, w, h);
    let got = colour_bounds(&v, w, h, notice.colour)
        .expect("the failure message reached no pixels at all");
    assert!(
        got.0 >= nx - 0.5
            && got.0 + got.2 <= nx + nw + 0.5
            && got.1 >= ny - 0.5
            && got.1 + got.3 <= ny + nh + 0.5,
        "the failure text drew at {got:?}, outside its notice rect {:?}",
        (nx, ny, nw, nh)
    );
    // Wrapped, not merely cut: one line's box is a single glyph tall.
    assert!(
        got.3 > LINE_H,
        "the message was cut to one line instead of wrapped: box {got:?}"
    );

    // The control. Same text, same detector, a column twice the canvas wide.
    let mut unbounded = accounts_failed_frame(&body);
    unbounded
        .notice
        .as_mut()
        .expect("the control still has a notice")
        .w = w * 2.0;
    let cv = geometry(&unbounded, w, h);
    let control = colour_bounds(&cv, w, h, notice.colour)
        .expect("the control drew nothing, so it proves nothing");
    assert!(
        control.0 + control.2 > nx + nw,
        "the detector cannot see an overflow: control box {control:?} against rect {:?}",
        (nx, ny, nw, nh)
    );
}

#[test]
fn wrap_bounded_breaks_a_run_that_no_whitespace_wrap_could() {
    // The difference from `wrap_measured` in one test, with that function as
    // the control: what makes a second wrap necessary rather than a flag on
    // the first is that the multiplayer screen's greedy fallback ("a word that
    // does not fit starts a line") does nothing at all for a 400-character
    // token.
    let b = Quads::new(854.0, 480.0);
    let run = "x".repeat(400);
    let column = 120.0;

    let hard = wrap_bounded(&b, &run, column, 8);
    assert!(hard.len() > 1, "the run was not broken at all: {hard:?}");
    for (i, line) in hard.iter().enumerate() {
        let lw = b.text_width(line, 1.0);
        assert!(lw <= column, "line {i} measures {lw} in a {column} column");
    }

    let soft = wrap_measured(&b, &run, column, 8);
    assert_eq!(
        soft.len(),
        1,
        "wrap_measured's documented behaviour changed: {soft:?}"
    );
    assert!(
        b.text_width(&soft[0], 1.0) > column,
        "the control did not overflow, so it proves nothing"
    );

    // And it terminates on a column too narrow for a single glyph, rather
    // than pushing empty lines forever.
    let starved = wrap_bounded(&b, &run, 1.0, 4);
    assert_eq!(starved.len(), 4);
    assert!(starved.iter().all(|l| l.chars().count() == 1));
}

/// A server MOTD padded with leading spaces (a common way to fake centring
/// on a fixed-width client) must keep them — `split_whitespace` would
/// otherwise discard the whole leading run as mere word separation, which is
/// indistinguishable from "the server sent no padding at all" once dropped.
///
/// The discriminating input, per this fix's own reasoning: a one-line MOTD
/// coincides under both hypotheses if it starts with a *non*-space
/// character, so this pads with real leading spaces and checks they land in
/// the output verbatim.
#[test]
fn wrap_measured_preserves_a_paragraphs_leading_spaces() {
    let b = Quads::new(854.0, 480.0);
    // 3 leading spaces + "Hello" comfortably fits one line at this width.
    let motd = "   Hello";
    let lines = wrap_measured(&b, motd, 200.0, 2);
    assert_eq!(
        lines,
        vec!["   Hello".to_string()],
        "the leading padding must survive into the wrapped line, not be \
         trimmed to \"Hello\""
    );
}

/// A second-page paragraph's leading spaces must survive too, not only the
/// very first paragraph in the string — the leading-whitespace restoration
/// is per-paragraph, not a one-shot fixup of the whole MOTD.
#[test]
fn wrap_measured_preserves_leading_spaces_after_a_newline() {
    let b = Quads::new(854.0, 480.0);
    let motd = "Title\n  Subtitle";
    let lines = wrap_measured(&b, motd, 200.0, 2);
    assert_eq!(lines, vec!["Title".to_string(), "  Subtitle".to_string()]);
}

/// **The line-cap bug.** A two-line MOTD whose second line, correctly
/// wrapped, holds three words — the discriminating input the guard's old
/// position (checked before every word, rather than only when a *new* line
/// is about to be pushed) could not survive: the moment `out.len()` reached
/// `max_lines` it returned immediately, truncating the last visible line to
/// whichever single word had just opened it. A one-word second line passes
/// either way and proves nothing.
///
/// Widths are exact, not eyeballed: the hermetic `Quads` with no font
/// attached measures every character (including a join space) at a fixed
/// `(GLYPH_W + 1) = 6px` advance (`text_px`/`text_w`). Six two-letter words
/// at `max_px = 50.0`: `"aa bb cc"` is 8 chars = 48px (fits), `"aa bb cc
/// dd"` is 11 chars = 66px (does not) — so line 1 is forced to stop at
/// three words and line 2 must be able to grow past its first word to
/// prove the fix, not merely reach two lines.
#[test]
fn wrap_measured_keeps_filling_the_last_visible_line_past_the_cap() {
    let b = Quads::new(854.0, 480.0);
    let motd = "aa bb cc dd ee ff";
    let max_px = 50.0;

    // Predictions from outside the function under test, using the same
    // fixed-advance arithmetic `text_px` uses (6px per character).
    assert_eq!(b.text_width("aa bb cc", 1.0), 48.0);
    assert_eq!(b.text_width("aa bb cc dd", 1.0), 66.0);

    let lines = wrap_measured(&b, motd, max_px, 2);
    assert_eq!(
        lines,
        vec!["aa bb cc".to_string(), "dd ee ff".to_string()],
        "the second line must keep accepting words that fit it after the \
         line cap is reached, not stop at its first word"
    );
}

/// **Owner's report**: "sometimes the spacing is missing for text (generally
/// when its coloured), it might be ignoring a space when its preceded by a
/// colour". `restyle_wrapped` re-attaches `wrap_measured`'s plain-string
/// output to the original styled spans by walking both in lockstep and
/// probing forward for each character; a space sitting exactly at a colour
/// change is the one input that makes that probe cross a style boundary
/// instead of staying inside one run. Two placements, both real: the space
/// as the *trailing* character of the red run and as the *leading* character
/// of the blue one — a bug that only dropped one direction would still pass
/// a test that only tried the other.
#[test]
fn restyle_wrapped_keeps_a_space_sitting_at_a_colour_boundary() {
    use lodestone_model::TextColor;

    let red = TextStyle {
        color: Some(TextColor::Red),
        ..TextStyle::default()
    };
    let blue = TextStyle {
        color: Some(TextColor::Blue),
        ..TextStyle::default()
    };

    // Case 1: the space trails the red run ("Hello " + "World").
    let trailing = [
        TextSpan { text: "Hello ".to_string(), style: red },
        TextSpan { text: "World".to_string(), style: blue },
    ];
    let rows = restyle_wrapped(&trailing, &["Hello World".to_string()]);
    assert_eq!(rows.len(), 1);
    let joined: String = rows[0].iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        joined, "Hello World",
        "a space trailing the coloured run must survive: got {rows:?}"
    );
    assert!(
        rows[0].iter().any(|s| s.text.contains(' ')),
        "the space must appear in *some* run's text, not vanish entirely: {rows:?}"
    );

    // Case 2: the space leads the blue run ("Hello" + " World").
    let leading = [
        TextSpan { text: "Hello".to_string(), style: red },
        TextSpan { text: " World".to_string(), style: blue },
    ];
    let rows = restyle_wrapped(&leading, &["Hello World".to_string()]);
    assert_eq!(rows.len(), 1);
    let joined: String = rows[0].iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        joined, "Hello World",
        "a space leading the coloured run must survive too: got {rows:?}"
    );
    assert!(
        rows[0].iter().any(|s| s.text.contains(' ')),
        "the space must appear in *some* run's text, not vanish entirely: {rows:?}"
    );
}

/// One wheel notch on the accounts list moves **18 px**, through the generic router.
///
/// The magnitude is the claim, not the direction: "it scrolled" is satisfied by the
/// row-index model this replaced, which is the defect the owner reported. So the
/// prediction is separated from both rivals, each computed from outside constants:
///
/// | hypothesis | one notch |
/// |---|---|
/// | vanilla, `floor(defaultEntryHeight / 2)` | **18** |
/// | the row-index model this replaced, one notch one row | 36 |
/// | a whole band, if the notch were mistaken for a page | 147 |
///
/// It then asserts 18 lands strictly *inside* row 0 (which spans 0..36 in content
/// space) and coincides with **no** row top, so a snap-to-row implementation cannot
/// pass. Driven through `MenuNav::scroll_active_list` — the router `app`'s single
/// `MouseWheel` arm calls — rather than through `AccountsNav::scroll_by`, because the
/// router is the part that was missing: before it, `app/` had exactly two wheel arms
/// and neither was this screen's.
#[test]
fn one_notch_on_the_accounts_list_is_half_a_row_through_the_generic_router() {
    const CANVAS_H: f32 = 240.0;
    let mut nav = accounts_nav("acct-notch", &["a", "b", "c", "d", "e", "f", "g", "h"]);
    let mut ui = crate::menu::UiState::default();
    ui.open_accounts();
    assert_eq!(
        nav.accounts().scroll(),
        0.0,
        "precondition: a freshly opened list is at the top"
    );

    // Negative `dy` scrolls down, matching vanilla's sign (the negation lives in
    // `ScrollList::mouse_scrolled`, so this is winit's `scrollY` verbatim).
    let moved = nav.scroll_active_list(&ui, -1.0, CANVAS_H);
    assert!(moved, "the router must report that the accounts list moved");
    let one = nav.accounts().scroll();

    assert_eq!(one, 18.0, "one notch is floor(36 / 2), not a whole row");
    assert_ne!(one, 36.0, "the row-index model's answer must be excluded");
    assert_ne!(one, 147.0, "a page-sized notch must be excluded");

    // Strictly inside row 0, and on no row's top — the property a snap-to-row
    // implementation structurally cannot have. Row tops are derived from the same
    // helper the draw places rows with.
    let band_top = crate::menu::render::accounts_band_top();
    assert!(
        (0..9).all(|i| crate::menu::render::accounts_row_top(i, one) != band_top),
        "offset {one} coincides with a row top, so it is indistinguishable from a jump"
    );
    assert!(
        one > 0.0 && one < 36.0,
        "offset {one} must sit strictly inside the first row"
    );

    // Three notches reach 54 — not a multiple of 36, so no row counter can represent
    // it at all. This is the assertion that cannot be satisfied by rescaling a
    // row-quantized implementation.
    nav.scroll_active_list(&ui, -2.0, CANVAS_H);
    let three = nav.accounts().scroll();
    assert_eq!(three, 54.0, "three notches of travel");
    assert_ne!(three % 36.0, 0.0, "54 must not be expressible as whole rows");

    // And the clamp is the primitive's: scrolling far past the end lands exactly on
    // `max_scroll`, computed here from vanilla's own expression rather than read back.
    nav.scroll_active_list(&ui, -1000.0, CANVAS_H);
    let content = 9.0 * 36.0 + 2.0 * widget::LIST_CONTENT_PADDING;
    let band = CANVAS_H - 60.0 - 33.0;
    assert_eq!(
        nav.accounts().scroll(),
        content - band,
        "the clamp must be vanilla's own max-scroll-amount accessor = its own content-height accessor - height"
    );

    // Scrolling up past the top clamps at zero rather than going negative.
    nav.scroll_active_list(&ui, 1000.0, CANVAS_H);
    assert_eq!(nav.accounts().scroll(), 0.0, "the top clamp is zero");
}

/// The router answers `false` for a screen with no list, which is what lets `app`
/// have **one** wheel arm instead of one per screen.
///
/// The control for the test above: without this, `scroll_active_list` returning
/// `true` unconditionally would still pass every assertion there while making the
/// wheel do something on screens that have no list.
#[test]
fn the_wheel_router_declines_a_screen_with_no_list() {
    let mut nav = accounts_nav("router-decline", &["a", "b", "c", "d", "e", "f", "g", "h"]);
    let mut ui = crate::menu::UiState::default();
    // The title screen is `owns_frame`, so `app`'s arm *does* fire here — which is
    // exactly why the router rather than the arm has to be the thing that declines.
    assert!(
        owns_frame(ui.screen()),
        "premise: the title screen is inside the set app's wheel arm covers"
    );
    assert!(
        nav.active_list(&ui).is_none(),
        "the title screen must declare no list"
    );
    assert!(
        !nav.scroll_active_list(&ui, -1.0, 240.0),
        "the router must decline a screen with no list"
    );
    // And the accounts list, reached from the same nav, still moves — so the `false`
    // above is the screen's answer and not a broken router.
    ui.open_accounts();
    assert!(
        nav.scroll_active_list(&ui, -1.0, 240.0),
        "the same router must still move a screen that does have a list"
    );
}

/// **The accounts screen has a scrollbar, and it is the multiplayer list's.**
///
/// This is the gate for the generic `ListSpec` hook, and it is a pixel gate on
/// purpose: the hook's whole reason for existing is that a screen adopting
/// `ScrollList` before it landed would have had correct geometry, green unit tests
/// and *nothing on screen* — `render::draw` called `server_scroll_list` by name, so
/// only one screen could ever have a bar. So the claim is not "the geometry is
/// right", it is "pixels appear in the scrollbar's rect on a screen that is not the
/// multiplayer list".
///
/// Measured **by location and by colour**, never as a frame fraction:
///
/// - the thumb's rect carries `LABEL`, the colour `draw_scrollbar` gives the scroller
/// - the 8 px gutter between the rows' right edge and the bar carries nothing **but
///   the band's own tint**, which is what pins the bar *outside* the row column
///   (vanilla's own scroll-bar-x accessor equals its own row-right accessor plus its
///   own scrollbar-width accessor plus 2) rather than inset into it
/// - a list short enough not to scroll draws **no bar at all** — vanilla's
///   `if (this.scrollable())` gate, and the control that stops the two assertions
///   above passing on a bar that is unconditionally painted
///
/// Every rect comes from `ScrollList::scrollbar_rects`, the same call the draw makes,
/// rather than from restated arithmetic.
#[test]
fn the_accounts_screen_draws_the_same_scrollbar_the_server_list_does() {
    let (w, h) = (854.0, 240.0);
    let nav = accounts_nav_scrolled("acct-bar", 8, 18.0);
    let mut ui = crate::menu::UiState::default();
    ui.open_accounts();
    let statuses =
        crate::menu::status::StatusCache::with_probe(crate::menu::status::unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("the accounts screen owns its frame");
    let v = geometry(&f, w, h);

    let spec = f
        .list
        .as_ref()
        .expect("frame_for must stamp the accounts screen's ListSpec");
    let list = spec.model(h).expect("nine 36 px rows in a 147 px band");
    let row_right = spec.row_right(w);
    let (track, thumb) = list
        .scrollbar_rects(row_right)
        .expect("premise: nine rows in a 147 px band must scroll");

    let on_thumb = coverage_of(&v, w, h, thumb, LABEL);
    assert!(
        on_thumb > 0.90,
        "the scroller is not painted at {thumb:?}: only {on_thumb} of it carries LABEL \
         (track {track:?}, row right {row_right})"
    );

    // The gutter: `scrollbar_x` is `row_right + 6 + 2`, so the 8 px immediately right
    // of the rows belongs to neither. Derived from the same expression, not restated.
    //
    // **Re-derived when the band chrome landed**, and the old form is worth recording
    // because it was correct and is now the wrong question. This asserted
    // `coverage(...) == 0` — nothing at all in the gutter — which held only because the
    // band had no background: vanilla's own list-background extraction blits `menu_list_background.png`
    // across the list widget's own x-through-right span, and that is the whole canvas,
    // so the gutter is *inside* the tint by vanilla's own construction. Asserting
    // emptiness again would mean asserting the tint away.
    //
    // The claim the gate exists to make is unchanged and is now spelled colour-wise:
    // the **topmost** thing in the gutter is the tint at every sample, so no row ink
    // and no scrollbar ink reached it. Strictly stronger than the old count — a bar
    // inset into the row column would paint `LABEL` here and fail — and it cannot be
    // satisfied by a frame that simply drew nothing, because a bare backdrop reports
    // `None` rather than the tint.
    let gutter = (row_right, list.top(), widget::SCROLLBAR_WIDTH + 2.0, list.height());
    let tinted = coverage_of(&v, w, h, gutter, LIST_BAND_TINT);
    assert_eq!(
        tinted, 1.0,
        "the gutter {gutter:?} between the rows and the bar is not uniformly the band \
         tint ({tinted} of it is) — something else painted there, so the bar is inset \
         into the row column rather than outside it"
    );

    // The control, run rather than described: two accounts is three rows, which fit
    // the band, so `scrollable()` is false and the bar must vanish entirely. If this
    // still found LABEL in the same rect, the assertion above would be measuring a
    // bar that is always drawn — which is what "the bar exists" must not mean.
    let short_nav = accounts_nav("acct-bar-short", &["A", "B"]);
    let short_f =
        frame_for(&ui, &short_nav, &statuses, &mut fav).expect("still the accounts screen");
    let short_v = geometry(&short_f, w, h);
    let short_spec = short_f.list.as_ref().expect("a short list still declares a spec");
    let short_list = short_spec.model(h).expect("and still has a band");
    assert!(
        !short_list.scrollable(),
        "premise: three 36 px rows must fit a {} px band",
        short_list.height()
    );
    assert!(
        short_list.scrollbar_rects(short_spec.row_right(w)).is_none(),
        "a list that does not scroll must report no scrollbar rects"
    );
    let on_thumb_short = coverage_of(&short_v, w, h, thumb, LABEL);
    assert_eq!(
        on_thumb_short, 0.0,
        "a non-scrolling list still painted {on_thumb_short} of {thumb:?} in LABEL"
    );
}

/// A straddling account row is **cut at the band**, not drawn over the footer.
///
/// ## Why this replaced `a_short_canvas_truncates_the_account_window_…`
///
/// That test asserted the opposite rule, and its premise expired the moment the
/// list went pixel-granular. It required `accounts_row_visible` to reject any row
/// not wholly inside the band, and checked the survivors ended above the arranged
/// button row. Both halves are now false *by design*: at an intermediate offset a
/// straddling row is the normal case, and rejecting it would drop a row at every
/// position between two whole-row stops — a worse artefact than the 36 px stepping
/// the conversion removed. `draw_account_entry` is wrapped in `Quads::with_clip`
/// instead, so the row is drawn **and cut**.
///
/// Note how it would have failed: the old premise assertion `fitting < VISIBLE_ROWS`
/// goes false (all five rows now "fit" the partial-overlap test), so the test would
/// have failed loudly rather than silently passing — which is why this is a rewrite
/// and not a deletion. The rule worth keeping is the *consequence* the old test was
/// really about, and it is the stronger claim: **no account row paints below the
/// band**, whatever the offset.
///
/// Measured by location, in the rows' own 305 px column, at an offset deliberately
/// chosen to put a row across the boundary. Failure prints the offending band.
#[test]
fn an_account_row_straddling_the_band_is_clipped_not_drawn_over_the_footer() {
    let (w, h) = (854.0, 240.0);
    // Nine logical rows in a 147 px band: the list scrolls, so an intermediate
    // offset is reachable. 18 px is one wheel notch — half a row, which guarantees
    // some row crosses each edge of the band.
    let nav = accounts_nav_scrolled("clip-band", 8, 18.0);
    // **Through `frame_for`, not `accounts_idle_frame`.** The spec is stamped by
    // `frame_for`'s tail, the same place `gui_scale` is, so calling the per-screen
    // builder directly gets a frame with `list: None` — which is exactly the island
    // this whole hook exists to prevent, and worth exercising rather than working
    // around. This assertion is therefore also the guard that the stamp happens.
    let mut ui = crate::menu::UiState::default();
    ui.open_accounts();
    let statuses = crate::menu::status::StatusCache::with_probe(
        crate::menu::status::unavailable_probe(),
    );
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("the accounts screen owns its frame");
    let v = geometry(&f, w, h);

    let spec = f
        .list
        .as_ref()
        .expect("frame_for must stamp the accounts screen's ListSpec");
    let list = spec.model(h).expect("and it has a band at 240 px");
    let band_bottom = list.top() + list.height();
    let col_x = spec.row_left(w);
    let col_w = spec.row_w(w);

    // Precondition, executed rather than assumed: a row really does straddle the
    // bottom edge at this offset. Without it this test could pass on a list that
    // simply ends above the band.
    let straddler = (0..9).find(|&i| {
        let (_, y, _, rh) = accounts_row_rect(i, w, 18.0);
        y < band_bottom && y + rh > band_bottom
    });
    assert!(
        straddler.is_some(),
        "premise: no row crosses the band bottom at {band_bottom}, so the clip is untested"
    );

    // The claim: nothing the list draws lands below the band, in the list's own
    // column.
    //
    // **The strip is band-bottom to the *button row's* top, not to the canvas
    // bottom, and getting that wrong is instructive.** The first version of this
    // measured all the way down and read 0.352 covered — which looks exactly like a
    // broken clip and is in fact the four action buttons, which legitimately own the
    // footer band. A control has to ask what *else* already paints in the rect before
    // it can attribute coverage to the thing under test; here 0.352 is almost
    // precisely the button row's own 20 px out of the 60 px band. The button y comes
    // off `accounts_button_slot` — the same arranged slot the draw places the buttons
    // from — rather than being restated as a constant.
    //
    // **The strip now starts `SEPARATOR_H` below the band, and that is a re-derivation
    // rather than a loosening.** Vanilla's own list-separator extraction blits `footer_separator.png`
    // — 32×2 — at exactly its own bottom accessor, so the first two rows of this strip belong to
    // the separator by vanilla's own construction, and the old bound of `band_bottom`
    // asserted them away. It read 0.083 covered when the chrome landed, which is 2 of
    // the 24 sample rows: the bar, not a row. The clip under test is unchanged.
    let (_, button_y, _, _) = accounts_button_slot(0).resolve(w, h);
    assert!(
        button_y > band_bottom + SEPARATOR_H,
        "premise: the button row at {button_y} must sit below the band at {band_bottom} \
         plus its separator"
    );
    let below = (
        col_x,
        band_bottom + SEPARATOR_H,
        col_w,
        button_y - band_bottom - SEPARATOR_H,
    );
    let spill = coverage(&v, w, h, below);
    assert_eq!(
        spill, 0.0,
        "an account row painted {spill} of the gap {below:?} between the band bottom \
         ({band_bottom}) and the button row ({button_y}); straddling row {straddler:?}"
    );

    // The control this needs: the clip must not be achieving that by drawing
    // nothing at all. The band itself has to be covered.
    let inside = (col_x, list.top(), col_w, list.height());
    let drawn = coverage(&v, w, h, inside);
    assert!(
        drawn > 0.10,
        "the band {inside:?} is nearly empty ({drawn}), so the zero above proves \
         only that the list is not drawing"
    );
}

