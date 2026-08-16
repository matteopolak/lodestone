# The accounts screen

## What it is

`Screen::Accounts` — the account switcher and its sign-in sub-flow — rebuilt to
look like the multiplayer server list: a real `HeaderAndFooterLayout`, list rows
at a 36 px pitch in a 305 px column, a footer of `LinearLayout`-arranged
nine-slice buttons, and **no text that can run off the screen**.

The brain is unchanged and lives in
[`menu/accounts.rs`](../crates/lodestone-shell/src/menu/accounts.rs) — which
accounts exist, which is highlighted, the sign-in state machine, the worker
thread. What changed is the presentation, in
[`menu/render.rs`](../crates/lodestone-shell/src/menu/render.rs), plus one
primitive (`MenuNotice`) that did not exist.

**There is no accounts screen in vanilla.** Minecraft picks an account in the
launcher, outside the game, which is also why the title-screen button that opens
this is documented as non-vanilla (`nav::MainButton::Accounts`). So the reference
for the geometry is *this repo's own* `JoinMultiplayerScreen` port — see
[`server-list.md`](./server-list.md), which is where every number below actually
comes from. Nothing here was read out of the jar for this screen, and the
constants say so individually.

| file | what it owns |
|---|---|
| `menu/accounts.rs` | the metadata, the highlight/focus/scroll indices, `SignIn`, the worker |
| `menu/render.rs` | `accounts_layout` … `accounts_row_rect`, `accounts_*_frame`, `draw_account_entry`, `MenuNotice` |
| `menu/nav.rs` | `key_accounts`, and the `hover`/`click` arms that route row indices in |

## The reported bug, and the fix

A player reported that the sign-in error text was **too large to read and ran off
the screen**. It was drawn through `MenuFrame::message`: one `to_uppercase`d
string, centred at `TEXT_SCALE` (2.0, so 12 px per glyph in the fallback font),
with no wrap and no clip. At that scale a 60-character message is already wider
than an 854 px canvas.

Shortening the message was the immediate fix and happened separately, in
`lodestone-auth`. It is **not** a fix for the screen, because the screen does not
control the string:

- `AuthError`'s `Service`/`Xsts` variants carry a snippet of whatever Microsoft or
  Mojang actually returned — `step_result` formats `"{status}: {snippet}"` with up
  to 400 characters of response body;
- the loopback flow's `verification_uri` is a few hundred characters of OAuth
  query string;
- `save_error` is an OS keychain or filesystem error string.

All three are JSON or URLs, which means **no whitespace**. A wrap that only breaks
on spaces does nothing for them, and the multiplayer screen's `wrap_measured` is
exactly that wrap by design (its greedy fallback, "a word that does not fit starts
a line", still emits one 2400 px line).

So the failure state, the sign-in URL and the save error all go through a new
`MenuFrame::notice`:

```rust
pub struct MenuNotice {
    pub text: String,       // unwrapped, arbitrarily long, not ours
    pub origin: Origin,
    pub dx: f32, pub dy: f32,
    pub w: f32,             // the wrap column
    pub bottom: f32,        // px kept clear at the bottom of the canvas
    pub colour: [f32; 4],
}
```

Two properties matter:

- **The text is carried, the lines are not.** Wrapping is measured in the font the
  draw will use, so it happens in `build` — the same reason
  `ServerEntryView::motd` is carried unwrapped.
- **The line count is not carried either.** `bottom` says how much of the canvas
  to keep clear and `notice_rect` turns that into however many whole `LINE_H`
  lines fit. The *layout* decides how much text a canvas shows; a constant would
  decide it once for every canvas, and be wrong on the short ones.

`wrap_bounded` is the wrap. It is greedy on whitespace like `wrap_measured`, and
additionally **breaks inside a word** wider than the column, via `clip_measured`.
It is deliberately a second function rather than a flag on the first: the
no-mid-word-break behaviour is a documented fidelity choice on the multiplayer
screen and must not change under it, and for a notice the same behaviour is not a
rounding error but the bug. (`wrap_bounded` is in fact *closer* to vanilla —
`StringSplitter` breaks mid-word too.) Its single-glyph guard is load-bearing: at
a column narrower than one character `clip_measured` returns `""`, and pushing
empty lines forever is how this would hang instead of draw.

## The geometry

`accounts_layout(width, height)` is a real
`HeaderAndFooterLayout::with_heights(w, h, 33, 60)` with

- the title as a **zero-width** `StringWidget` cell in the header (there is no font
  at arrange time; the header frame centres its child, so a zero-width cell lands
  on the centre a real-width one would be centred about);
- a `SpacerElement` sized to `content_height()` in the contents, so the list takes
  part in the measurement and is never drawn;
- one `LinearLayout::horizontal().spacing(4)` of four 74 px buttons in the footer.

Read back once at `ACCOUNTS_REF_CANVAS` (854×480) into `AccountsBlock`, and every
footer rect re-expressed as a `Slot` from `Origin::ScreenBottom`.
`the_accounts_slots_do_not_depend_on_the_reference_canvas` re-arranges at three
canvases and requires each slot to be identical **and** to resolve onto that
canvas' own arrangement. Even widths only, for the same half-pixel reason the
world-select footer has.

Two choices worth knowing:

- **The footer band is 60 px for a single 20 px row.** The `FrameLayout` splits the
  40 px of slack 20/20, and the lower half is where the key-hint line goes
  (`accounts_hint_dy` derives it from the arranged row, +8 px). A 33 px band would
  put that line off the bottom of the canvas.
- **Four buttons at 74 px** measure `4*74 + 3*4 = 308`, which is the multiplayer
  screen's *lower* footer row exactly. That is why the two screens' footers line
  up rather than each being centred to its own width, and it is asserted rather
  than described.

Row geometry, all mirroring the server list:

```
row       = (floor(w/2) - 152, 33 + 2 + rendered*36, 305, 36)
content   = row inset by 2 a side  ->  (…, …, 301, 32)
head      = content origin, 32x32          (the content box's full height)
name      = contentX + 32 + 3, contentY + 1        (white, clipped)
detail    = same x, contentY + 12                  (-8355712, clipped)
"Selected"= right-aligned at contentRight - 5, contentY + 1
```

`floor(w/2) - floor(305/2)` is **two separate integer divisions**, which a `Slot`
cannot express — so `row_rect` answers an account row from
`accounts_row_rect(view.index, width)` before it looks at `slot`, exactly as it
does for a multiplayer entry. Answering it *inside* `row_rect` is the point: that
function is also `app.rs`'s hit-test, so the draw and the click cannot disagree.

`AccountEntryView::index` is the row's position **in the rendered window**, not in
the account list — the frame builder slices by the scroll offset before building
rows, so there is no scroll term in the geometry.

### Two cursors, one screen

Same split the server list needed, for the same reason: both are visible at once
and they are drawn completely differently.

| fact | where it lives | how it draws |
|---|---|---|
| which account the list cursor is on | `AccountsNav::highlighted` → `AccountEntryView::selected` | 1 px outline, black interior |
| which footer button the mouse is on | `AccountsNav::focus` past the end of the list → `MenuFrame::selected` | `widget/button_highlighted` |

`MenuFrame::selected` is `usize::MAX` whenever focus is on a row, which highlights
no button. Before this change the account list used one index for both and the row
highlight was the generic row-stack fill.

### Scrolling: what changed and what did not

`accounts.rs`'s `VISIBLE_ROWS` is still **5, still a count, and still not derived
from the canvas** — that module has no canvas, which is the same gap #402 records
for the server and world lists. What this change adds is the second half:
`accounts_row_visible` refuses to *draw* a row that would not fit whole between
the content band and the footer, so a short canvas truncates the window instead of
painting a half row over the four action buttons.

The residual gap is bounded and is the same one `server-list.md` records:
`row_rect` still answers for a skipped row, so a click there selects it and
nothing else. Raising `VISIBLE_ROWS` is only safe once the window itself is
canvas-derived, which needs `frame_for` to know the canvas.

## How to change it

- **The notice's rect is `notice_rect`, and the gate calls it.** Do not restate the
  arithmetic in a test —`CLAUDE.md` records two gates whose *restated* rect was
  itself the thing that was wrong. Same for `accounts_hint_dy` and
  `accounts_wide_button_slot`, which both read the arranged button row rather than
  a constant.
- **A new state needs a title and a hint, not a new layout.**
  `accounts_title_label(text)` takes the string because the three states have three
  titles; `accounts_hint_label` is the key-hint line that `vanilla: true`
  suppresses `MenuFrame::footer` in favour of.
- **`MenuFrame::message` is still the row-stack screens' single-line error and is
  suppressed on a `vanilla` frame.** If a screen's message can be long or is not
  ours, it wants a `MenuNotice`, not `message`.
- **The row order is a coupling.** `AccountsNav::hover` maps a rendered row index
  through the scroll window and then onto the four button slots, so `shown` list
  rows followed by Add / Select / Remove-or-Edit-Name / Back is load-bearing.
  `the_account_rows_are_in_the_order_click_assumes` is the guard, the same shape
  the settings and multiplayer screens carry against the same #391 bug.
- **There must not be a fifth footer button.** Four 74 px buttons with
  `spacing(4)` measure 308, inside `config::MIN_SCALED_WIDTH`'s 320; five measure
  386 and hang 33 px off *each* edge at the smallest supported GUI scale. That is
  why the **third slot changes identity** instead — `Remove` on an account row,
  `Edit Name` on the offline row, which cannot be removed and so had a dead
  button there already. `AccountsNav::third_button` is the one expression the
  caption and `activate_button` share; do not give either its own copy of the
  predicate. The key-hint line follows the same call, because `Del remove` was
  false on that row.
- **The offline entry is not an account.** `selected.is_none()` *is* its selected
  state — read `accounts.rs`'s module docs before touching selection; there is no
  third state on disk and adding one is a `lodestone-auth` schema change. Its
  **label is the persisted offline name**, not a literal — see
  [`offline-identity.md`](./offline-identity.md) for the editor the third button
  opens and for why `with_path` derives `offline.json` from `profiles.json`'s own
  directory.
- **The name editor is a fourth frame, and `frame_for` must consult it.** The
  `Screen::Accounts` arm checks `name_edit_view()` before `sign_in_view()`, and it
  does so as a `match` **expression** rather than an early `return`: everything in
  `frame_for` feeds a tail `frame.map` that stamps `gui_scale` and `list`, so a
  `return` would produce one screen that silently ignored the GUI-scale setting.
  `frame_for_reaches_the_name_editor_and_still_stamps_the_frame` is the guard for
  both halves.
- **Do not move `AccountsNav::pump`.** It is driven from `render::frame_for`
  through a `&AccountsNav` with interior mutability, deliberately: `frame_for` is
  the one call site that runs every frame regardless of input, which is what lets
  "waiting for you to sign in" advance with no keystroke.
- **`WorkerMsg::Prompt` opens the browser.** `pump` turns it into an effect and the
  render thread launches the browser from it. Do not add a second opener; that
  already shipped a double-open once.
- **And therefore: a *test* that sends a `Prompt` and pumps opens a real browser.**
  That shipped too — see [the unrequested browser window](#the-unrequested-browser-window)
  below. `open_in_browser` is now a `cfg(test)`/`cfg(not(test))` **fork**, so no unit
  test can reach the OS handoff. Do not collapse it back into one function with a
  `cfg!(test)` early return: the fork is what makes the interception assertable.
  Use `.invalid` hostnames in fixtures regardless.
- **No field on this screen may hold a credential.** The user code and the
  verification URL are the only strings it displays; sign-in happens on
  Microsoft's own page.

## Two smaller fixes that came with it

- **`Enter` now cancels an in-flight sign-in**, as well as `Escape`. The sign-in
  state has exactly one control — Cancel — and `MenuNav::click`'s default
  translation is `hover` + `Enter`, so without this the new button would draw,
  highlight, and do nothing: #391's shape.
- **`AccountsNav::hover` is inert while a sign-in is in flight or has failed.**
  Those states draw a different frame with no list rows, so a row index there means
  nothing to the scroll-window mapping — applying it anyway moved the account
  cursor when the player clicked "Cancel".
- **`finish_ms_token` logs its failure.** That is the arm a real sign-in failure
  takes, and it was silent: the `tracing::warn!` in `run_browser_login` covers a
  *different* arm (the pre-browser poll). `AuthError`'s `Debug` carries the step and
  the untruncated body that `describe_auth_error` flattens to one sentence, and the
  on-screen string is transient and uncopyable, so this line is the only thing that
  makes a failure diagnosable after the fact.

## The unrequested browser window

Reported twice from play: `https://login.live.com/oauth20_remoteconnect.srf` kept
opening in the owner's browser, unprompted, with no visit to the accounts screen.

**It was the test suite, not the game.**
`add_account_button_starts_the_flow_and_a_prompt_message_shows_it` fed the state
machine a `WorkerMsg::Prompt` whose `verification_uri` was the literal
`https://microsoft.com/link`, then called `pump` — and `pump` performs the open as
an *effect*, through `std::process::Command::new("open")`. From the OS's point of
view a unit test and a player pressing **Add account** are the same event. So every
`cargo test -p lodestone-shell` launched a browser window, and with several agents
running that suite continuously the windows arrived at random while the owner
played. `microsoft.com/link` 301s to `login.live.com/oauth20_remoteconnect.srf`,
which is why the URL was Microsoft's **device-code** page even though production
has used the loopback flow since `c33e325` and `run_device_code_login` has no
callers at all.

Three things in this made it hard to see, and each is the general lesson:

- **The URL pointed at a flow that does not run.** The device-code endpoint is
  reachable *only* from a test fixture string. An earlier pass reasoned from the
  URL to "the owner must be on a stale binary" — and the owner had reported a
  loopback-flow bug that same morning, so they demonstrably were not.
- **Nothing re-entered `Requesting`.** The frequency looked like a per-frame loop,
  and there is none: `activate_button(BUTTON_ADD)` is the only entry, `Failed` and
  `Idle` are terminal, and no error or timeout path restarts. The repetition was
  *one* open per test run, many test runs.
- **Microsoft was never contacted.** The fixture path hand-feeds a channel, so no
  device code was ever requested and there is no rate-limit exposure — the OS
  handoff was the entire cost.

Measured, with a shim named `open` placed first on `PATH` so no window could
actually appear: before, one `OPEN_CALLED https://microsoft.com/link` per lib-test
run; after, the shim log does not exist across all 955 lib tests.

`super::telemetry`'s **Privacy Statement** and **Give Feedback** buttons call the
same function and were a latent second instance — they had simply never been
activated by a test. The fork covers them.

A second, smaller instance of the same symptom was fixed alongside it:
`run_browser_login` sends its `Prompt` *before* the loop that first checks the
cancel flag, so pressing Cancel in that window still opened a window the user had
just refused. `pump_locked` now suppresses the URL effect when `cancel` is already
set; the worker's `Cancelled` still returns the screen to `Idle`.

## How it is proved

- **The overflow, by location.** A 396-character message with **no whitespace in
  it** (a JSON body, which is what `step_result` actually produces) must draw
  entirely inside `notice_rect`'s own rect, and the failure output is a bounding
  box rather than a fraction. Two further conditions keep it from being vacuous:
  the box must be more than one line tall (so a clip cannot pass as a wrap), and
  the **control is executed** — the same detector on the same frame with a wrap
  column twice the canvas wide must report a box outside the rect.
- **`wrap_bounded` against `wrap_measured` as its control**, which must produce one
  over-wide line for the same input; plus the starved-column case, which must
  terminate.
- **Row pixels by location**: each drawn row's rect must be covered, the row past
  the end must be empty, and the 32 px head must fill the content box's square.
- **Canvas independence** at three canvases, with each slot required to resolve
  onto that canvas' own arrangement rather than merely to compare equal.
- **The short-canvas guard against the arranged button row's own y**, not against
  its own formula, with both premises asserted (some rows fit, not all do) and a
  full-size canvas as the control.
- **The row order** the click path assumes, against the frame the draw builds.
- **At most one browser open per user action**, counted across 50 `pump` calls with
  two controls: 20 empty pumps must open nothing (so the count is not "one per
  frame"), and a *second* Add account must be allowed its own single open (so
  "still exactly one" cannot pass on a permanently dead recorder). Plus
  `the_real_browser_handoff_is_unreachable_from_a_unit_test`, which asserts the
  `cfg(test)` arm is the one compiled — the gate that would have caught the
  report above.

Not proved: nothing here has been through a GPU gate or a live sign-in. The
frames, the geometry and the wrap are all hermetic; what a real Microsoft failure
looks like on screen still needs an actual failed sign-in.

## Configuration

None of its own. `gui_scale` sets the logical canvas through
`render::logical_canvas`; `LODESTONE_DATA_DIR` moves `profiles.json`;
`LODESTONE_MS_CLIENT_ID` optionally overrides the shipped Azure client id
Add Account authenticates with (see [`accounts.md`](./accounts.md)).

## Dependencies

- `menu/{widget,layout,focus}.rs` — `Widget`/`WidgetSprites`,
  `HeaderAndFooterLayout`/`LinearLayout`/`FrameLayout`, and the `KeyEvent`
  boundary. See [`ui-framework.md`](./ui-framework.md),
  [`menu-widgets.md`](./menu-widgets.md), [`menu-layout.md`](./menu-layout.md).
- `lodestone-auth` — `AccountsMetadata`, `AccountSecrets`, the OAuth flow and
  `AuthError`. See [`accounts.md`](./accounts.md).
- [`server-list.md`](./server-list.md) — the screen this one is modelled on, and
  the source of every geometry constant.
