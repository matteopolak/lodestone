use super::{MenuNav, *};

/// The frame that is **actually on screen**, however it got there — the one
/// source `app.rs`'s mouse hit-test (`menu_row_at`) may consult.
///
/// # Why this exists rather than a call to `render::frame_for`
///
/// A player report (2026-08-04, "i cant click anything in the options menu")
/// was exactly this function's absence. `render::frame_for` is the authority on
/// which screens the *menu renderer owns* — screens it draws with a `Clear`
/// pass, replacing the world — and it deliberately answers `None` for the
/// screens that draw as an **overlay** over a still-rendering world. There are
/// now three of those: `Screen::Paused`, `Screen::Death`, and — since
/// `d096de8` — `Screen::Settings` when [`UiState::settings_in_world`], which
/// was made an overlay so that in-world Options stopped drawing the title
/// screen's panorama behind itself.
///
/// `menu_row_at` consulted `frame_for` with a `?`. Pause and death had each
/// been given their own branch there when they became overlays; the third was
/// not, so in-world Options had **no frame to hit-test against at all** and
/// every click returned `None` before it reached a row. Nothing was wrong with
/// the options screen's own geometry: the title-screen copy of the very same
/// rows hit-tests correctly (`clicking_an_options_row_at_its_own_coordinates_
/// activates_that_row` measures it), which is why the geometry was the wrong
/// place to look.
///
/// So the fix is not a fourth branch — it is putting the branch set *somewhere a
/// test can reach*, because three `if`s inlined in a private `app.rs` method
/// cannot be enumerated from anywhere, which is why the third one could go
/// missing silently. [`crate::menu::render::owns_frame`] says which screens
/// route mouse input; this says where their rows come from; and
/// `every_mouse_routable_screen_has_a_frame_to_hit_test` asserts the two sets
/// agree, so the *next* overlay screen fails a test instead of losing its
/// clicks.
///
/// Returns `None` only when no menu-ish screen is up at all — the same meaning
/// `frame_for`'s `None` had at the call site.
#[must_use]
pub fn on_screen_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
    death_message: Option<&[lodestone_model::text::InteractiveTextSpan]>,
    statuses: &super::status::StatusCache,
    favicons: &mut super::render::FaviconCache,
) -> Option<super::render::MenuFrame<'a>> {
    if ui.is_paused() {
        return Some(super::render::pause_frame(nav));
    }
    if ui.is_death() {
        return Some(super::render::death_frame(nav, death_message));
    }
    // The third overlay screen, and the one whose absence was the bug. Built
    // from exactly the call `app.rs`'s redraw uses to *draw* it, so the frame
    // the click hit-tests against is the frame on the glass — a second
    // construction here is how a click lands on a row the draw put elsewhere.
    if let Some(frame) = settings_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The Statistics screen — always reached from the pause menu (see
    // `stats_overlay_frame`'s own doc), so an overlay unconditionally.
    if let Some(frame) = stats_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The Server Links screen — always reached from the pause menu, so
    // (unlike in-world Settings) it has no out-of-world case at all and is an
    // overlay unconditionally. See `server_links_overlay_frame`'s own doc.
    if let Some(frame) = server_links_overlay_frame(ui, nav) {
        return Some(frame);
    }
    if let Some(frame) = friends_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The fourth overlay screen, and the second instance of the exact
    // shape above. `command_block_overlay_frame` is the *same call* the draw
    // path in `app/redraw.rs` makes — see its own doc for why it is a function
    // rather than a second construction here.
    if let Some(frame) = command_block_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The fifth overlay screen, same shape as the fourth immediately above.
    if let Some(frame) = sign_edit_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The sixth overlay screen, same shape again.
    if let Some(frame) = resource_pack_prompt_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The seventh overlay screen (`EditBook`), same shape again.
    if let Some(frame) = book_edit_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The eighth overlay screen (`TeleportToEntity`
    // remainder), same shape again.
    if let Some(frame) = spectator_menu_overlay_frame(ui, nav) {
        return Some(frame);
    }
    super::render::frame_for(ui, nav, statuses, favicons)
}

/// The sign-editing screen's overlay frame, or `None` when that screen is not
/// up — one expression with two consumers, exactly [`command_block_overlay_frame`]'s
/// shape and for the same reason: [`on_screen_frame`] hit-tests a click
/// against this, and `app/redraw.rs`'s overlay block draws it, so a second
/// construction anywhere would be free to disagree with it.
#[must_use]
pub fn sign_edit_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if !ui.is_sign_edit_open() {
        return None;
    }
    let state = nav.sign_edit()?;
    Some(super::render::sign_edit_frame(state))
}

/// The book-editing screen's overlay frame, or `None` when that screen is not
/// up — [`sign_edit_overlay_frame`]'s exact shape and for the same reason:
/// [`on_screen_frame`] hit-tests a click against this, and `app/redraw.rs`'s
/// overlay block draws it, so a second construction anywhere would be free
/// to disagree with it.
#[must_use]
pub fn book_edit_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    // **Both book screens, one function.** `app/redraw.rs`'s overlay block
    // calls this once by name, and the read-only `Screen::BookView` needs a
    // draw for exactly the same reason `Screen::BookEdit` does
    // (`menu::render::frame_for` has no arm for an overlay). Folding the
    // second screen in here rather than adding a ninth overlay block keeps
    // the "one construction, two consumers" property this function exists
    // for — `on_screen_frame` hit-tests clicks against whatever this
    // returns — and the two screens are mutually exclusive by construction,
    // since a stack is either writable or written.
    if ui.is_book_view_open() {
        return nav.book_view().map(super::render::book_view_frame);
    }
    if !ui.is_book_edit_open() {
        return None;
    }
    let state = nav.book_edit()?;
    Some(super::render::book_edit_frame(state))
}

/// The Spectator Menu's overlay frame, or `None` when that screen is not up
/// (`TeleportToEntity` remainder) — [`book_edit_overlay_frame`]'s
/// exact shape and for the same reason: [`on_screen_frame`] hit-tests a
/// click against this, and `app/redraw.rs`'s overlay block draws it, so a
/// second construction anywhere would be free to disagree with it. Unlike
/// `book_edit_overlay_frame` this never returns `None` merely because the
/// nav state is absent — [`MenuNav::spectator_menu`] always has one (see its
/// own field doc) — only because the screen itself is not showing.
#[must_use]
pub fn spectator_menu_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if !ui.is_spectator_menu_open() {
        return None;
    }
    Some(super::render::spectator_menu_frame(nav.spectator_menu()))
}

/// The resource-pack prompt's overlay frame, or `None` when it is not up —
/// the sixth overlay screen, [`sign_edit_overlay_frame`]'s exact shape and
/// for the same reason: a second construction in `app/redraw.rs`'s draw
/// block would be free to disagree with what this hit-tests against.
///
/// **Two bugs this used to carry, found auditing it alongside the Social fix
/// above.** [`crate::menu::confirm::resource_pack_prompt_frame`] builds its
/// `MenuFrame` with `..Default::default()`, so — unlike every other overlay
/// builder in this file — nothing here called [`super::render::stamp_canvas_facts`]
/// or touched `backdrop`, which left `backdrop` at `MenuFrame::default()`'s
/// `MenuBackdrop::Panorama`. `Panorama.wants_panorama()` is `true`, so
/// `MenuRenderer::draw` drew vanilla's cubemap over whatever `render_overlay`'s
/// `Load` op had already put in `view` — the paused world, when the prompt
/// opened from [`Screen::Playing`]/[`Screen::Chat`]/[`Screen::Container`]/
/// [`Screen::Paused`] — the exact defect class `stats_overlay_frame`'s own
/// doc records, and the general sweep in `render/tests.rs` could not catch
/// it: this screen is reached only through `ui.begin(SessionKind::Multiplayer)`
/// (the *no*-world case, where `Panorama` happens to be correct), never
/// through a live world, so `owns_frame_agrees_with_frame_for_on_every_screen`
/// walked straight past the broken case.
///
/// The record settles which is right: `Screen.extractBackground` forks on
/// `this.minecraft.level == null` — panorama with no level, the in-world
/// wash otherwise — exactly [`UiState::settings_in_world`]'s own fork, so
/// this now mirrors [`settings_overlay_frame`] instead of
/// [`sign_edit_overlay_frame`]: `Dim` (plus the blur — `PackConfirmScreen`
/// does not override `isInGameUi()`) when
/// [`UiState::resource_pack_prompt_in_world`], the untouched `Panorama`
/// default otherwise (`Screen::Connecting` has no level, matching vanilla's
/// `level == null` arm). Vanilla actually blurs there too — the `blur`
/// call in `extractBackground` is unconditional once `isInGameUi()` is
/// ruled out, panorama or not — but this port scopes the blur pass to
/// [`MenuRenderer::render_overlay`] frames only (see `render::blur`'s module
/// doc), so the Connecting-screen panorama stays unblurred, a stated cut
/// rather than an oversight.
#[must_use]
pub fn resource_pack_prompt_overlay_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
) -> Option<super::render::MenuFrame<'a>> {
    if !ui.is_resource_pack_prompt() {
        return None;
    }
    let prompt = nav.resource_pack_prompt()?;
    let mut frame = crate::menu::confirm::resource_pack_prompt_frame(prompt);
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    if ui.resource_pack_prompt_in_world() {
        frame.backdrop = super::render::MenuBackdrop::Dim;
        frame.blur = true;
    }
    Some(frame)
}

/// The **in-world** settings screen's overlay frame, or `None` when settings is not
/// up in a world — one expression with three consumers ([`on_screen_frame`]'s
/// hit-test, `app/redraw.rs`'s overlay draw, and the gate that measures it).
///
/// # Why this exists
///
/// A player report (2026-08-09): *"the main menu settings have the header/footer,
/// but if i go in game and open settings it doesnt have it. they should be the
/// exact same menu, not separate"*. They **are** the same screen — one
/// `Screen::Settings`, one [`crate::menu::options::settings_frame`] taking no
/// in-world flag, and an `active_list` arm keyed only on the page — so nothing
/// about the *content* differed. What differed is that
/// [`crate::menu::render::frame_for`] stamps the canvas facts onto everything it
/// returns and answers `None` for the overlay screens by design, so the in-world
/// path built `settings_frame` raw and never got them.
///
/// Measured on `SettingsPage::Sound` at 320×240 — the page and canvas the report
/// names — the raw call yields `list: None` where `frame_for` yields `Some`, and the
/// band tint and both bevelled separator bars are gated on
/// `ListSpec::chrome_rect`, so the whole chrome silently vanished. `cursor` was
/// dropped the same way, which is the in-world hover tooltips.
///
/// The draw path was **not** the difference and is worth recording as a ruled-out
/// hypothesis: `MenuRenderer::render` and `render_overlay` share one `draw` body
/// and differ only in the pass's load op, so both emit the chrome identically.
///
/// # The one thing that is legitimately context-dependent
///
/// [`crate::menu::render::MenuBackdrop::Dim`], and it is set here rather than in
/// `settings_frame`. Out of a world the settings tree sits on the panorama; in one
/// it must leave the paused world visible, which is vanilla's own fork
/// (`OptionsScreen` over the level vs over the title). `settings_frame` defaults to
/// `Panorama` and nothing was overriding it, so in-world Options drew the panorama
/// *over* the paused world — the same 2026-08-04 report that made this an overlay
/// in the first place, still live because routing the frame to `render_overlay`
/// changed the load op and not the frame's own backdrop declaration. `pause_frame`
/// and `death_frame` — the sibling overlays — set `Dim` by hand; this is the third.
///
/// The root page's `World Options...` row is **kept** context-dependent on purpose:
/// that is vanilla's `inWorld` header fork and it is about rows, not chrome.
#[must_use]
pub fn settings_overlay_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
) -> Option<super::render::MenuFrame<'a>> {
    if !(ui.is_settings() && ui.settings_in_world()) {
        return None;
    }
    let mut frame = crate::menu::options::settings_frame(
        nav.settings(),
        nav.options(),
        nav.options_save_error(),
    );
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    // `OptionsScreen` does not override `isInGameUi()` either, so vanilla
    // blurs behind in-world Options — see `MenuFrame::blur`'s own doc.
    frame.blur = true;
    Some(frame)
}

/// The Statistics screen's overlay frame, or `None` when it is not up —
/// [`settings_overlay_frame`]'s exact shape, but for a screen with no
/// out-of-world case at all: `UiState::open_statistics_from_pause` only
/// opens `Screen::Statistics` from `Screen::Paused`, and there is no
/// title-screen entry point (see that variant's own doc), so this is
/// unconditional rather than gated on an `..._in_world()` predicate the way
/// [`settings_overlay_frame`] is.
///
/// `Dim`, not the default `Panorama` — the fix for the defect
/// [`super::render::dispatch::frame_for`]'s `Screen::Statistics` arm
/// documents at length: a frame built outside `frame_for`'s own `Some` arm
/// never receives its stamp, so the in-world backdrop has to be set here by
/// hand, the same one line [`settings_overlay_frame`]/[`pause_frame`]/
/// [`death_frame`] already carry.
#[must_use]
pub fn stats_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if ui.screen() != Screen::Statistics {
        return None;
    }
    let mut frame = crate::menu::stats::frame(nav.stats(), nav.stats_snapshot());
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    // `StatsScreen` does not override `isInGameUi()` — see `MenuFrame::blur`'s
    // own doc.
    frame.blur = true;
    Some(frame)
}

/// The Social Interactions screen's overlay frame, or `None` when it is not
/// up — [`stats_overlay_frame`]'s exact shape and for the identical reason:
/// [`UiState::open_social_from_pause`] only opens [`Screen::Social`] from
/// [`Screen::Paused`] and there is no title-screen entry point (see that
/// variant's own doc), so this is unconditional too.
///
/// A player report (2026-08-15) caught this for Statistics; Social has the
/// same defect for the same underlying reason — [`super::render::dispatch::frame_for`]'s
/// old `Screen::Social` arm built `super::social::frame(..)` unconditionally,
/// which routes through `draw_menu`'s `Clear` pass and by construction never
/// renders the world that frame, so no backdrop value that arm's frame ever
/// carried could have shown the paused world behind it. `Dim`, not the
/// default `Panorama`, and stamped with the same canvas facts
/// [`stats_overlay_frame`] carries — a frame built outside `frame_for`'s own
/// `Some` arm gets neither for free.
#[must_use]
pub fn social_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if ui.screen() != Screen::Social {
        return None;
    }
    let mut frame = crate::menu::social::frame(nav.social(), ui.kind());
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    // `SocialInteractionsScreen` does not override `isInGameUi()` either.
    frame.blur = true;
    Some(frame)
}

/// Friends opened from pause is an overlay; the title route stays in the
/// ordinary full-frame dispatcher.
#[must_use]
pub fn friends_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if !ui.friends_in_world() {
        return None;
    }
    let mut frame = crate::menu::friends::frame(nav.friends());
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    frame.blur = true;
    Some(frame)
}

/// The Server Links screen's overlay frame, or `None` when it is not up —
/// one expression with two consumers, [`settings_overlay_frame`]'s exact
/// shape and for the same underlying reason: this screen can only ever be
/// reached from the pause menu (see [`super::Screen::ServerLinks`]'s own
/// doc), so it is an overlay unconditionally rather than conditionally like
/// in-world Settings. That is also why [`super::render::frame_for`] carries
/// no `Screen::ServerLinks` arm at all — every case is the overlay case,
/// so there is nothing for that dispatcher to build.
///
/// `Dim`, not the default `Panorama` — the same fix
/// [`settings_overlay_frame`]'s own doc explains at length: a frame built
/// without going through `frame_for`'s stamp has to set the in-world
/// backdrop by hand, or the paused world it must leave visible gets replaced
/// by the panorama that belongs to the *main menu* only.
#[must_use]
pub fn server_links_overlay_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
) -> Option<super::render::MenuFrame<'a>> {
    if ui.screen() != Screen::ServerLinks {
        return None;
    }
    let mut frame = crate::menu::server_links::frame(&nav.server_links);
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    // This client has no dedicated `ServerLinksScreen` in vanilla to check —
    // it stands in for `Dialogs.SERVER_LINKS`, a dialog over the pause
    // screen, which inherits `PauseScreen`'s own non-`isInGameUi` fork. See
    // `MenuFrame::blur`'s own doc.
    frame.blur = true;
    Some(frame)
}

/// The command block edit screen's overlay frame, or `None` when that screen is
/// not up — **one expression with two consumers**.
///
/// [`on_screen_frame`] hit-tests a click against this, and `app/redraw.rs`'s
/// overlay block draws it. Keeping one frame builder for both consumers makes
/// their geometry identical; separate constructions could place a click target
/// on a different row from the one the renderer displays.
///
/// The in-world Settings arm above still constructs `settings_frame` twice for
/// compatibility with its existing frame flow; this overlay has one shared
/// construction.
///
/// `nav.command_tree()` rather than `None`: the suggestion popup is fed by the
/// tree the server actually sent (the packet decoder supplies it and the input
/// path routes it here), and the popup is part of the frame the click has to
/// hit-test against.
#[must_use]
pub fn command_block_overlay_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
) -> Option<super::render::MenuFrame<'a>> {
    if !ui.is_command_block_open() {
        return None;
    }
    let state = nav.command_block()?;
    Some(super::render::command_block_frame(
        state,
        nav.command_tree(),
    ))
}

/// Whether the mouse and keyboard are routed to the menu layer rather than to
/// gameplay — the predicate `app/lifecycle.rs` guards its `CursorMoved`,
/// `MouseInput` and `KeyGate::menu` arms on.
///
/// # Why this is a function and not three copies of one expression
///
/// It was three copies. `render::owns_frame(screen) || ui.is_paused() ||
/// ui.is_death()` appeared literally in the hover guard, the click guard and
/// the `KeyGate` construction, and a *fourth* copy appeared in
/// `every_mouse_routable_screen_has_a_frame_to_hit_test` — which is precisely
/// why a copied predicate could miss this overlay. The gate now derives its
/// result from the same production routing rule, so adding or omitting a
/// routable screen changes both the predicate and this frame check, so they
/// cannot silently diverge.
///
/// With the rule named once, the gate's `routable` premise and the production
/// guard are the same code, so a screen the driver routes to and
/// [`on_screen_frame`] has no frame for is a test failure rather than a silent
/// dropped click.
///
/// `Screen::CommandBlockEdit` is in the set for the same reason `Paused` and
/// `Death` are: it is an overlay ([`render::owns_frame`](super::render::
/// owns_frame) is `false` for it, deliberately — the world keeps rendering
/// behind it, matching vanilla's `isInGameUi() == true`) with its own rows to
/// hover, click and type into. Without it the screen opened, and neither a
/// click nor a keystroke ever reached it.
///
/// Not a `UiState` method: `owns_frame` lives in `render`, so putting this on
/// `UiState` would make `menu.rs` depend on the renderer to answer an input
/// question.
#[must_use]
pub fn routes_menu_input(ui: &UiState) -> bool {
    super::render::owns_frame(ui.screen())
        || ui.is_paused()
        || ui.is_death()
        || ui.is_command_block_open()
        || ui.is_sign_edit_open()
        // Same reasoning as `is_command_block_open`/`is_sign_edit_open`
        // immediately above: `Screen::BookEdit` is `owns_frame == false`
        // (see [`book_edit_overlay_frame`]'s own doc) with its own rows to
        // hover, click and type into. Without this arm the screen would open
        // and never receive a single keystroke or click.
        || ui.is_book_edit_open()
        // The signed-book reader is its own overlay screen, but shares the
        // editor's row hit-test path. Without this arm arrow and Done clicks
        // fall through to gameplay before the book frame can receive them.
        || ui.is_book_view_open()
        // Advancements is not `owns_frame` (it is an overlay drawn
        // through `ContainerRenderer`), but Escape has to close it, and the
        // `_` arm of `MenuNav::key` routes exactly that through
        // `UiState::on_escape`.
        || ui.is_advancements()
        // Same reasoning as `is_command_block_open`/`is_sign_edit_open`
        // immediately above: without this arm a click or keystroke while the
        // prompt is up would fall through to gameplay input (mining,
        // movement) instead of answering the dialog — the screen would open,
        // draw, and never receive a single Accept/Decline.
        || ui.is_resource_pack_prompt()
        || ui.screen() == Screen::Friends
        // Server Links (like the `Screen::ResourcePackPrompt` arm above) is
        // `owns_frame == false` unconditionally — it is never routed through
        // the Clear pass, only ever drawn as an overlay — so without this arm
        // every click and keystroke on it would fall straight through to
        // gameplay input instead of reaching the screen at all.
        || ui.screen() == Screen::ServerLinks
}

/// Steps `i` one row in `forward`'s direction, wrapping, and keeps stepping
/// while the row it lands on is disabled.
///
/// This is vanilla's own focus rule: `AbstractWidget::nextFocusPath` returns
/// `null` for an inactive widget, so keyboard
/// navigation never *lands* on a greyed-out button — which is what makes it safe
/// to reproduce vanilla's full widget list with most of it disabled without the
/// arrow keys walking through five dead rows.
///
/// Returns `i` unchanged when nothing in `0..len` is enabled. Neither real
/// button set can be in that state, but the loop bound is what keeps a future
/// all-disabled set from spinning forever rather than being a latent hang.
pub(super) fn step_enabled(i: usize, len: usize, forward: bool, enabled: &dyn Fn(usize) -> bool) -> usize {
    if len == 0 {
        return 0;
    }
    let mut next = i.min(len - 1);
    for _ in 0..len {
        next = if forward {
            wrap_next(next, len)
        } else {
            wrap_prev(next, len)
        };
        if enabled(next) {
            return next;
        }
    }
    i
}

pub(super) fn wrap_next(i: usize, len: usize) -> usize {
    if len == 0 { 0 } else { (i + 1) % len }
}

pub(super) fn wrap_prev(i: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else if i == 0 {
        len - 1
    } else {
        i - 1
    }
}
