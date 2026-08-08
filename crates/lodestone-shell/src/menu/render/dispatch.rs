//! [`server_list_frame`] and [`frame_for`] — the single place menu *state*
//! becomes menu *content*, and the per-screen dispatch it does.
//!
//! Not named `frame_for`, which would collide with the `pub use` of the
//! function of that name in the module root.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;
use super::account_screen::{
    accounts_failed_frame, accounts_flow_frame, accounts_idle_frame, accounts_name_edit_frame,
};
use super::measure::{MANAGE_SERVER_TITLE_Y, manage_server_slot};
use super::screens::{COPYRIGHT, credits_frame, error_frame, loading_frame, version_line};
use super::server_list::SERVER_LIST_FOOTER_H;

/// Builds vanilla's `JoinMultiplayerScreen` (#396): one row per saved server at
/// `ServerSelectionList`'s geometry, then the seven footer buttons.
///
/// ## What each row's state resolves to
///
/// The MOTD column is vanilla's `serverData.motd`, which the pinger *overwrites*
/// per state rather than keeping alongside the real MOTD: it is
/// `multiplayer.status.pinging` while a probe is in flight
/// (`ServerStatusPinger.java:65`) and the red `CANT_CONNECT_MESSAGE` when one
/// fails (`:168`). So a failed row shows its reason in the MOTD line and an empty
/// status column (`:169` sets `status` to empty), which is exactly where this
/// screen already put it.
///
/// The one row state that is **ours** is [`super::status::StatusSlot::Idle`] — a
/// row nothing has probed yet. Vanilla has no such state for longer than a frame,
/// so it has no text for it; this shows the address, which is the only thing
/// known about a server before it answers, and is what this screen showed for
/// every row before #396.
///
/// ## Selection, and vanilla's null
///
/// `JoinMultiplayerScreen.onSelectedChange` starts with **nothing** selected and
/// three inactive buttons (`:246-257`). This shell has a keyboard row cursor that
/// always points somewhere, so "has a selection" is modelled as "the list is not
/// empty" — see [`super::nav::ServerListButton::enabled`], which is where that
/// deviation is argued.
#[must_use]
fn server_list_frame(
    nav: &super::nav::MenuNav,
    statuses: &super::status::StatusCache,
    favicons: &mut FaviconCache,
) -> MenuFrame<'static> {
    use super::nav::SERVER_LIST_BUTTONS;
    use super::status::{self, ServerState, StatusCache, StatusSlot};

    let entries = nav.list().entries();
    let last = entries.len().saturating_sub(1);
    // One clock read for the whole frame, so every pinging row animates in step
    // (out of phase by index, which is `pinging_sprite`'s own doing).
    let millis = statuses.millis();
    // #402: read once and stamp onto every entry — see `ServerEntryView::scroll`.
    let scroll = nav.server_scroll();

    let mut rows: Vec<MenuRow> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let slot = statuses.get(e);
            let state = slot.state(status::STATUS_PROTOCOL);
            // Only a real status carries server styling; the other three MOTDs
            // are this client's own strings and stay flat by construction.
            let (motd, motd_is_error, motd_spans) = match slot {
                StatusSlot::Idle => (e.address_label(), false, Vec::new()),
                StatusSlot::Pending => (status::PINGING_MOTD.to_string(), false, Vec::new()),
                StatusSlot::Ok(s) => (s.motd.clone(), false, s.motd_spans.clone()),
                StatusSlot::Failed(why) => (why.clone(), true, Vec::new()),
            };
            let (status_text, status_is_error) = match (state, slot) {
                (ServerState::Successful, StatusSlot::Ok(s)) => (s.players.clone(), false),
                // An incompatible server shows its *version* where a compatible
                // one shows its player count (`:344-346`), which is the whole
                // point: the row says what it speaks, in red.
                (ServerState::Incompatible, StatusSlot::Ok(s)) => (s.version.clone(), true),
                _ => (String::new(), false),
            };
            let latency = match slot {
                StatusSlot::Ok(s) => s.latency_ms,
                _ => None,
            };
            MenuRow {
                label: e.name.clone(),
                favicon: match slot {
                    StatusSlot::Ok(s) => s
                        .favicon_png
                        .as_deref()
                        .and_then(|png| favicons.get(&StatusCache::key(e), png)),
                    _ => None,
                },
                enabled: true,
                // No `slot`: a list row's left edge is `floor(width / 2) - 152`,
                // Java integer division on *each* term, which a `Slot`'s
                // `anchor + dx` cannot express (see `server_row_left`). `row_rect`
                // resolves it from `entry.index` instead, which keeps the draw and
                // `app.rs`'s hit-test on one definition all the same.
                entry: Some(ServerEntryView {
                    index: i,
                    motd,
                    motd_spans,
                    motd_is_error,
                    status: status_text,
                    status_is_error,
                    status_sprite: status::status_sprite(state, latency, millis, i),
                    // Vanilla's `onlinePlayersTooltip` — set for SUCCESSFUL and
                    // INCOMPATIBLE rows in `refreshStatus`
                    // (`ServerSelectionList.java:410,430`), never for INITIAL,
                    // PINGING or UNREACHABLE.
                    online_players: match (state, slot) {
                        (
                            ServerState::Successful | ServerState::Incompatible,
                            StatusSlot::Ok(s),
                        ) => status::player_sample_lines(&s.sample, s.online),
                        _ => Vec::new(),
                    },
                    selected: i == nav.server_index(),
                    can_move_up: i > 0,
                    can_move_down: i < last,
                    scroll,
                }),
                ..Default::default()
            }
        })
        .collect();

    // `onSelectedChange`'s three conditional buttons plus the four unconditional
    // ones, in the order they are added to the two footer rows (`:68-125`).
    let has_selection = !entries.is_empty();
    for button in SERVER_LIST_BUTTONS {
        rows.push(MenuRow {
            label: button.label().to_string(),
            enabled: button.enabled(has_selection),
            slot: Some(server_list_footer_slot(button)),
            ..Default::default()
        });
    }

    let mut labels = vec![server_list_title_label()];
    // Not vanilla's: a failed `servers.json` write has no vanilla equivalent
    // (vanilla's `ServerList.save` swallows its own IOException into the log), and
    // a player who adds a server and sees it vanish deserves the reason. Placed
    // just above the footer band so it cannot collide with a row.
    if let Some(err) = nav.save_error() {
        labels.push(MenuLabel {
            text: err.to_uppercase(),
            origin: Origin::ScreenBottom,
            dx: 0.0,
            dy: -(SERVER_LIST_FOOTER_H + LINE_H + 2.0),
            align: Align::Centre,
            colour: FG_BAD,
            scale: 1.0,
        });
    }

    MenuFrame {
        rows,
        // On this screen `selected` is the **footer button** the cursor is over,
        // not the selected server: a list entry carries its own
        // `ServerEntryView::selected`, because vanilla draws the two completely
        // differently (a 1 px row outline versus `widget/button_highlighted`) and
        // both can be visible at once.
        selected: match nav.list_button() {
            Some(b) => entries.len() + b,
            None => usize::MAX,
        },
        vanilla: true,
        labels,
        cursor: nav.menu_cursor(),
        ..Default::default()
    }
}

/// Builds the frame for whichever menu screen `ui` is on.
///
/// This is the single place menu *state* becomes menu *content*, so the app has
/// no per-screen branching and a test can assert what each screen shows without
/// a GPU. Returns `None` for any screen this renderer does not own, which is the
/// app's signal to render the world instead.
#[must_use]
pub fn frame_for<'a>(
    ui: &super::UiState,
    nav: &super::nav::MenuNav,
    statuses: &super::status::StatusCache,
    favicons: &mut FaviconCache,
) -> Option<MenuFrame<'a>> {
    use super::Screen;
    use super::nav::{FormField, MAIN_BUTTONS};

    let frame = match ui.screen() {
        // Vanilla's `TitleScreen`: the logo pair, eight widgets at vanilla's
        // rects (see `title_slot`) with two of them present-and-disabled
        // (Realms, Friends — Language/Accessibility joined the live set once
        // their destination screens were built, see `MainButton::Language`'s
        // own doc), and the two corner strings. No big "LODESTONE" heading and
        // no key-hint footer — the logo *is* the heading, and vanilla draws no
        // footer.
        Screen::MainMenu => Some(MenuFrame {
            rows: MAIN_BUTTONS
                .iter()
                .map(|b| MenuRow {
                    label: b.label().to_string(),
                    enabled: b.enabled(),
                    slot: Some(title_slot(*b)),
                    icon: b.icon(),
                    ..Default::default()
                })
                .collect(),
            selected: nav.main_index(),
            vanilla: true,
            logo: true,
            labels: vec![
                MenuLabel {
                    text: version_line(),
                    origin: Origin::BottomLeft,
                    dx: 2.0,
                    dy: CORNER_TEXT_Y,
                    align: Align::Left,
                    colour: LABEL,
                    scale: 1.0,
                },
                MenuLabel {
                    text: COPYRIGHT.to_string(),
                    origin: Origin::BottomRight,
                    dx: -2.0,
                    dy: CORNER_TEXT_Y,
                    align: Align::Right,
                    colour: LABEL,
                    scale: 1.0,
                },
            ],
            ..Default::default()
        }),
        // Vanilla's `JoinMultiplayerScreen` (#396): a `HeaderAndFooterLayout`
        // title, the `ServerSelectionList`'s 36 px rows, and seven footer buttons
        // three of which are inactive with nothing selected. Built in its own
        // function because the row content alone is thirty lines of state
        // resolution — see `server_list_frame`.
        Screen::ServerList => Some(server_list_frame(nav, statuses, favicons)),
        // Vanilla's `ManageServerScreen` (the framework conversion this arm
        // used to lack entirely: no row here carried a `slot`, so every
        // widget drew through the pre-#392 centred stack instead of a real
        // `widget/button*`/`widget/text_field` sprite). See
        // `manage_server_slot` for the five widgets' vanilla rects.
        Screen::ServerEdit => {
            let form = nav.form();
            let title = if form.editing.is_some() {
                "Edit Server Info"
            } else {
                "Add Server"
            };
            // Vanilla disables Done rather than printing an error
            // (`ManageServerScreen.java:92-93`) — the greyed `widget/
            // button_disabled` sprite this row now draws *is* the feedback,
            // so no extra text duplicates it.
            let valid = form.is_valid();
            use super::nav::{ADDRESS_FIELD, CANCEL_ROW, DONE_ROW, NAME_FIELD, RESOURCE_PACK_ROW};
            Some(MenuFrame {
                // `edit` carries a **clone of the live widget**, which is how
                // #395's persistent `EditBox` reaches a draw through a `&MenuNav`
                // frame builder: `build`'s `draw_edit_box` moves the clone into
                // this frame's rect and asks it where the text, caret and
                // selection go. `label` stays populated because it is what
                // `the_edit_form_shows_both_fields_and_marks_the_focused_one` and
                // every other frame-shape test read; nothing draws it now.
                rows: vec![
                    MenuRow {
                        label: form.name().to_string(),
                        detail: "Server Name".to_string(),
                        enabled: true,
                        field: true,
                        edit: Some(form.fields.name.clone()),
                        slot: Some(manage_server_slot(NAME_FIELD)),
                        ..Default::default()
                    },
                    MenuRow {
                        label: form.address().to_string(),
                        detail: "Server Address".to_string(),
                        enabled: true,
                        field: true,
                        edit: Some(form.fields.address.clone()),
                        slot: Some(manage_server_slot(ADDRESS_FIELD)),
                        ..Default::default()
                    },
                    // Present and inactive — see `RESOURCE_PACK_ROW`'s doc on
                    // why: `ServerEntry` has no `pack_status` to cycle.
                    MenuRow {
                        label: "Server Resource Packs".to_string(),
                        enabled: false,
                        slot: Some(manage_server_slot(RESOURCE_PACK_ROW)),
                        ..Default::default()
                    },
                    MenuRow {
                        label: "Done".to_string(),
                        enabled: valid,
                        slot: Some(manage_server_slot(DONE_ROW)),
                        ..Default::default()
                    },
                    MenuRow {
                        label: "Cancel".to_string(),
                        enabled: true,
                        slot: Some(manage_server_slot(CANCEL_ROW)),
                        ..Default::default()
                    },
                ],
                selected: match form.field() {
                    FormField::Name => NAME_FIELD,
                    FormField::Address => ADDRESS_FIELD,
                },
                hovered: form.hovered_button(),
                vanilla: true,
                labels: vec![
                    MenuLabel {
                        text: title.to_string(),
                        origin: Origin::ScreenTop,
                        dx: 0.0,
                        dy: MANAGE_SERVER_TITLE_Y,
                        align: Align::Centre,
                        colour: LABEL,
                        scale: 1.0,
                    },
                    // Not vanilla — this client's own affordance, kept from
                    // the pre-conversion screen: SRV resolution and the
                    // name-falls-back-to-host rule have no vanilla widget to
                    // announce them (`ServerEntry::split_host_port`,
                    // `EditForm::to_entry`).
                    MenuLabel {
                        text: "Tab switches fields - an empty name uses the host".to_string(),
                        origin: Origin::ScreenBottom,
                        dx: 0.0,
                        dy: -16.0,
                        align: Align::Centre,
                        colour: FG_DIM,
                        scale: 1.0,
                    },
                ],
                ..Default::default()
            })
        }
        // Vanilla's `SelectWorldScreen` (issues #397, #287, then #468's real save
        // list): the title, the search box, the six footer buttons — three still
        // present and disabled — and **one row per world in `saves/`**. See
        // `super::world_select` for what is disabled and why, `world_select_slot`
        // for the footer geometry and `world_list_row_rect` for the rows'.
        //
        // **The row order is the focus-id order, not the on-screen order**, and it
        // has to be: `MenuFrame::selected`/`hovered` and `app.rs`'s hit-test all
        // index `rows` by focus id (`world_select`'s `SEARCH_FIELD`,
        // `FIRST_BUTTON_ROW`, `FIRST_WORLD_ROW`), so this pushes search → buttons →
        // worlds even though the worlds draw above the buttons. Getting it wrong is
        // #391's shape at list scale: every click one control off.
        Screen::WorldSelect => {
            use super::world_select::{FIRST_WORLD_ROW, WORLD_SELECT_BUTTONS};
            let ws = nav.world_select();
            let mut rows = Vec::with_capacity(1 + WORLD_SELECT_BUTTONS.len() + ws.shown_len());
            rows.push(MenuRow {
                // Not drawn: `draw_edit_box` reads the widget. Populated for the
                // same reason the edit form's is — the frame-shape tests read it.
                label: ws.search().value().to_string(),
                enabled: true,
                field: true,
                edit: Some(ws.search().clone()),
                slot: Some(world_select_search_slot()),
                ..Default::default()
            });
            for button in WORLD_SELECT_BUTTONS {
                rows.push(MenuRow {
                    label: button.label().to_string(),
                    // The **widget's** live flag, not `WorldSelectButton::enabled`
                    // — see `WorldSelectNav::is_active` on why asking the enum here
                    // would be a second source of truth.
                    enabled: ws.is_active(button.row()),
                    slot: Some(world_select_slot(button)),
                    ..Default::default()
                });
            }
            // One row per **filtered** world, in list order — the three text lines
            // come off `WorldSummary` here rather than in the draw, so the draw
            // decides nothing except where (the same division `ServerEntryView`
            // documents).
            // Read once and stamped onto every entry (#541) — see
            // `WorldEntryView::scroll`.
            let scroll = ws.scroll();
            for row in 0..ws.shown_len() {
                let world = ws
                    .world_at(row)
                    .expect("shown_len() rows are exactly the rows world_at answers");
                rows.push(MenuRow {
                    label: world.display_name.clone(),
                    detail: world.detail_line(),
                    trailing: world.info_line(),
                    // `LevelSummary.primaryActionActive` — a corrupt world's row is
                    // listed and not openable.
                    enabled: ws.is_active(FIRST_WORLD_ROW + row),
                    world: Some(crate::menu::render::WorldEntryView {
                        index: row,
                        selected: ws.selected_row() == Some(row),
                        scroll,
                    }),
                    ..Default::default()
                });
            }
            Some(MenuFrame {
                rows,
                // The *focused* row. `usize::MAX` when nothing is focused, which
                // highlights nothing (see `MenuFrame::selected`) — rather than
                // `0`, which would light the search field up whenever focus was
                // cleared.
                selected: ws.focused_row().unwrap_or(usize::MAX),
                hovered: ws.hovered(),
                vanilla: true,
                labels: {
                    let mut labels = vec![world_select_title_label()];
                    // `NoWorldsEntry` — the empty-list row, and **only** when the
                    // list really is empty. This is what keeps "no worlds" apart
                    // from "the list failed to draw": with no label at all the two
                    // are the same picture.
                    if let Some(text) = ws.empty_label() {
                        labels.push(world_list_row_label(text));
                    }
                    labels
                },
                message: ws.error().map(str::to_string),
                ..Default::default()
            })
        }
        // Vanilla's whole `OptionsScreen` tree (issue #55). This used to be two
        // hand-written rows in a centred stack with a key-hint footer; it is now
        // nine pages of `OptionsList` geometry built from a table, with the
        // controls this client does not honour drawn inactive. Every decision —
        // which page, which rows are visible, which are live, where each one
        // sits, and — since the Online page — whether the root's header button
        // is even a link at all — belongs to `super::options`; this arm only
        // supplies the two things that live outside it (`nav`, `options`).
        //
        // **`None` when `ui.settings_in_world()`** — a player report
        // (2026-08-04) caught that opening Options from the pause menu showed
        // the panorama instead of the paused world, because this arm always
        // returned `Some` and `owns_frame`'s `Clear` pass (`app.rs::draw_menu`)
        // has no idea a world is loaded behind it. The panorama is
        // `Screen::MainMenu`'s background alone (`panorama.rs`'s module docs);
        // in-world Options is vanilla's `OptionsScreen` opened over the paused
        // level, same shape as `Screen::Paused`/`Screen::Death`. Returning
        // `None` here routes it through the *world* render path in `app.rs`'s
        // `redraw` instead of `draw_menu`'s Clear pass — exactly like Paused
        // and Death — where a **new overlay block** (not yet landed; this is
        // the render-side half, app.rs's half is brokered) must draw this same
        // `settings_frame` with `MenuRenderer::render_overlay` after the world
        // paints, or the screen goes blank in-world until that block exists.
        // `owns_frame(Screen::Settings)` is deliberately left `true` regardless
        // — every non-render caller (mouse/keyboard routing) still wants
        // Settings treated as a menu-row screen whether or not a world is
        // behind it, so `the_root_title_is_centred_on_the_header_block`'s
        // sibling invariant, "`owns_frame` agrees with `frame_for`", now has
        // its one documented exception: see
        // `frame_for_defers_to_an_overlay_for_in_world_settings`.
        Screen::Settings if !ui.settings_in_world() => Some(super::options::settings_frame(
            nav.settings(),
            nav.options(),
            nav.options_save_error(),
        )),
        Screen::Settings => None,
        // The account list (issue #66). `pump` is called here, on every
        // frame this screen is showing, rather than from an `app.rs` hook —
        // see `accounts.rs`'s module docs on why that module is written to
        // work through a shared `&AccountsNav` reference.
        Screen::Accounts => {
            use super::accounts::SignInView;
            let accounts = nav.accounts();
            accounts.pump();
            // **The name editor is checked before the sign-in state, not folded
            // into it.** `SignInView` is about the Microsoft device-code flow;
            // adding a rename variant to it would make every `match` on that
            // enum answer a question it is not about. The editor is also
            // unreachable *while* a sign-in is in flight (`handle_key_with`
            // returns early in that state), so the two cannot both be open and
            // this ordering is a readability choice rather than a precedence
            // rule.
            //
            // **Not `return Some(..)`.** Everything in this function is a `let
            // frame = match ..` feeding the `frame.map` below, which stamps
            // `gui_scale` and `list` onto whatever comes out — an early return
            // would produce an editor that ignored the GUI-scale setting and had
            // no scrollbar declaration, silently and only on that one screen.
            Some(match accounts.name_edit_view() {
                Some(view) => accounts_name_edit_frame(&view),
                None => match accounts.sign_in_view() {
                    SignInView::Idle => accounts_idle_frame(accounts),
                    SignInView::Requesting => accounts_flow_frame(None, None, false),
                    SignInView::Waiting {
                        user_code,
                        verification_uri,
                    } => accounts_flow_frame(
                        // Empty means "no code to show", which is the loopback flow:
                        // the browser is already open at the URL and there is nothing
                        // to type. The device-code flow still fills both. `None` is a
                        // shape `accounts_flow_frame` already handles — see the
                        // `Requesting` arm above, which passes it for both.
                        (!user_code.is_empty()).then_some(user_code.as_str()),
                        Some(&verification_uri),
                        true,
                    ),
                    SignInView::Failed { message } => accounts_failed_frame(&message),
                },
            })
        }
        // The loading screen (issue #449): "Connecting..." over a flat dark
        // backdrop while the handshake/configuration phase runs. Safe to take
        // the whole frame here — no chunk packets arrive until after login, so
        // nothing meshes or uploads behind the loading screen and the world
        // path (`app::redraw`) is not needed under it. This supersedes the
        // older note that `Screen::Connecting` was deliberately absent so the
        // world "keeps rendering so chunks mesh and upload as they stream in":
        // that concern belongs to the *post-login* terrain stream, which stays
        // on the world path as an overlay in `app::redraw` (see its loading
        // block) rather than piling behind a full screen.
        Screen::Connecting => Some(loading_frame("Connecting...")),
        // The error screen is drawn by this renderer too, even though it is not
        // an `is_menu()` screen: a session that dies mid-game used to leave a
        // frozen world on screen with no explanation. See `error_frame` for the
        // vanilla `DisconnectedScreen` this now reproduces.
        Screen::Error => Some(error_frame(ui.error())),
        // The credits/end-poem screen (#192) — see `credits_frame`'s own doc
        // for why its content is a short placeholder rather than vanilla's
        // real auto-scrolling poem.
        Screen::Credits => Some(credits_frame()),
        // Social Interactions (#189) — see `super::social::frame`'s own doc
        // for the singleplayer/multiplayer fork.
        Screen::Social => Some(super::social::frame(nav.social(), ui.kind())),
        // Statistics (#188) — `StatsSnapshot::default()` is not a
        // placeholder, it is the only data that has ever existed: see
        // `super::stats`'s module docs on why nothing decodes the packet
        // that would populate one yet.
        Screen::Statistics => Some(super::stats::frame(
            nav.stats(),
            &super::stats::StatsSnapshot::default(),
        )),
        // World Creation (issue #190) — see `super::create_world`'s own doc
        // for why this is one flat hand-placed list rather than vanilla's
        // three tabs.
        Screen::CreateWorld => Some(super::create_world::frame(nav.create_world())),
        // Vanilla's `ConfirmScreen` (issue #540) — the gate the world list's
        // Delete button passes through. See `super::confirm`'s module doc for why
        // it is a screen at all rather than a two-press mode on the list, and
        // `confirm::frame` for the one thing this arm must not do: default
        // `selected` to `0`, which would light the affirmative button up.
        Screen::Confirm => Some(super::confirm::frame(nav.confirm())),
        _ => None,
    };
    // Stamped on every screen (not read back out of `nav` per-screen above) so
    // the whole menu scales, not only the settings screen that edits the
    // setting.
    frame.map(|mut f| {
        f.gui_scale = nav.gui_scale();
        // Stamped for the same reason and in the same place as `gui_scale`: a screen
        // that has a scrolling list must not also have to remember to tell the draw
        // about it. `MenuNav::active_list` is the one place that decides, so the
        // scrollbar the draw paints and the offset the wheel arm clamps are two
        // readers of one declaration rather than two declarations that agree today.
        f.list = nav.active_list(ui);
        f
    })
}

