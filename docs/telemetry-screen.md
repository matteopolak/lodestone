# Telemetry Data screen

## What it is

`SettingsPage::Telemetry` (issue #415): vanilla's `TelemetryInfoScreen`,
reached from the root settings grid's own "Telemetry Data..." button
(flipped from `no_screen` to `nav()`). Built as an honest prose screen: a
title, a two-paragraph description, and four buttons — two of them real.

## How it works

- `crates/lodestone-shell/src/menu/telemetry.rs` — the whole model:
  `TelemetryControl` (the four buttons), `TelemetryNav` (a plain cursor —
  no scroll, no search, no list), and `frame`.
- `crates/lodestone-shell/src/menu/options.rs` — `SettingsPage::Telemetry`,
  the `SettingsNav` plumbing, `settings_frame`'s early-return branch — the
  same shape `SettingsPage::KeyBinds`/`SettingsPage::Language` already
  established.
- `crates/lodestone-shell/src/menu/nav.rs` — `MenuNav::key_telemetry`,
  `apply_telemetry`, and the hover/click/key routing guards.
- `crates/lodestone-shell/src/menu/render.rs` — `Origin::Telemetry`. The
  footer reuses `Origin::Settings(Placement::Footer)` directly rather than
  a new variant, since it is geometrically identical to
  `SettingsPage::Accessibility`'s own two-button footer.

### Why this is honestly a prose screen

Vanilla's real screen has an opt-in checkbox and a live, scrollable
`TelemetryEventWidget` (the pending-telemetry-events log) in addition to the
prose and buttons. This client collects no telemetry at all — no
`TelemetryManager`, no event log, no opt-in state anywhere in the workspace.
Vanilla's own `EXTRA_TELEMETRY_AVAILABLE` conditional already omits the
checkbox when there is nothing to opt into; this client is permanently on
that branch, so leaving the checkbox out is not a reduction, it is the same
conditional vanilla already has, resolved one way for good. The event list
is the same shape: nothing could ever populate one, so it is correctly
absent rather than an empty stub.

## Wired vs. decorative

- **Wired**: reaching the screen and back (Escape/Done → Root), and —
  genuinely — **Privacy Statement** and **Give Feedback**: both open the
  real vanilla URLs (`CommonLinks.PRIVACY_STATEMENT`,
  `CommonLinks.RELEASE_FEEDBACK`) in the system browser via
  `super::accounts::open_in_browser` (made `pub(crate)` for this reuse). A
  link needs no telemetry state to exist.
- **Present-and-inactive**: **View My Data** — there is no telemetry log
  directory to open.
- **Correctly absent, not decorative**: the opt-in checkbox and the event
  list — see above.

## Configuration

None — this screen has no persisted state of its own.

## Dependencies

- `super::accounts::open_in_browser` — the OS-command URL opener.
- `super::options` — `FOOTER_HEIGHT`, `footer_rects`, `Placement::Footer`,
  reused for this screen's own two-button footer.
- `super::layout` — `HeaderAndFooterLayout`, `LinearLayout`, `widget_rects`
  for the header's title/description/button-row arrangement.
- The 26.2 jar's `assets/minecraft/lang/en_us.json`
  (`telemetry_info.screen.title`, `.description`,
  `.button.privacy_statement`, `.button.give_feedback`,
  `.button.show_data`) and `.cache/mc/26.2/client-src/net/minecraft/util/
  CommonLinks.java` for the two URLs.

## See also

- [Language screen](./language-screen.md), [Resource Packs screen](./resource-packs-screen.md)
  — the sibling screens issue #415 built alongside this one.
- [The settings tree](./settings-screen.md) — the root page this screen is
  reached from, and the census this page's nav button moves.
