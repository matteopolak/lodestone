# The menu UI framework

## What it is

The plan of record for vanilla-faithful **menu** screens: a widget type, layout containers, a
`Screen`-level input layer, and the three target screens — the multiplayer server list, world select
with creation disabled, and the full settings tree with every real button present and the
unsupported ones greyed out.

This is research, not implementation. The tracker is **#392** with children #393–#398 plus the
existing settings branch at #55. Everything below was measured against the 26.2 jar under
`.cache/mc/26.2/client-src` and against this tree; the point of writing it down is that four
separate agents each re-derived a piece of one Java class in a single day, and nothing on record
would have stopped a fifth.

## Why: one class, discovered four times

`AbstractContainerScreen` is 587 lines inherited by **23 concrete container screens** (17 direct
`extends`, 10 more through `AbstractRecipeBookScreen`, `ItemCombinerScreen`,
`AbstractMountInventoryScreen` and `AbstractFurnaceScreen`; 27 files in the hierarchy, 4 abstract).

| issue | what it re-derived | ACS member |
|---|---|---|
| #370 | container title text, colour, shadow, placement | `extractLabels` — `AbstractContainerScreen` |
| #376 | slot highlight back/front pair | `extractSlotHighlightBack`, `…Front` |
| #377 | carried-stack layering | `extractCarriedItem`, `extractFloatingItem` |
| #378 | number-key swap, and its ordering vs the hotbar keys | `checkHotbarKeyPressed` |

Each was correct work done well. The failure is not in any of them — it is that the shared mechanism
was never on record, so each rediscovery looked like new work. Porting the class once closes it.

The draw order is the specific thing that keeps getting rediscovered, and it is stated plainly in
`AbstractContainerScreen.extractContents`:

```
super.extractRenderState → extractLabels → extractSlotHighlightBack
                         → extractSlots → extractSlotHighlightFront
```

then `extractCarriedItem` and `extractTooltip` from `AbstractContainerScreen.extractRenderState`.

## The port is closer than it looks

26.2 has already made the split this codebase made. There is no immediate-mode
`render(GuiGraphics, …)` any more:

- `Renderable` is a **one-method interface** — `components/Renderable.java`:
  `void extractRenderState(GuiGraphicsExtractor graphics, int mouseX, int mouseY, float a)`.
- `GuiGraphicsExtractor` (`gui/GuiGraphicsExtractor.java`, 1453 lines) looks immediate-mode but
  is not: it appends `GuiElementRenderState` records into a `GuiRenderState`
  (`renderer/state/gui/GuiRenderState.java`) — `BlitRenderState`, `ColoredRectangleRenderState`,
  `GuiTextRenderState`, `GuiItemRenderState`, `PictureInPictureRenderState`.
- A later pass walks `forEachElement` / `forEachItem` / `forEachText` and draws.

That is our `ExtractSet`/`FrameSet` split, with the same shape and the same reason. The layering
primitive matches too: `Screen.extractRenderStateWithTooltipAndSubtitles` (`screens/Screen.java`)
brackets passes with `graphics.nextStratum()`, which is the same device as the stratum markers the
carried-stack work added, and `GuiRenderState.nextStratum` / `blurBeforeThisStratum` are the vanilla
side of it.

**Consequence for the port:** vanilla's `extract*` methods can be ported *as* extract methods,
one-to-one, rather than being rewritten into a different rendering model. This is the single
strongest argument for doing the work now rather than later — the structural distance is the
smallest it has ever been.

## Vanilla → us mapping

| vanilla | file | ours today | issue |
|---|---|---|---|
| `AbstractWidget`, `Button`, `AbstractButton` | `gui/components/` | nothing — buttons declared and hit-tested per screen in `menu/render.rs`, `menu/nav.rs` | #393 |
| `WidgetSprites.get(enabled, focused)` | `gui/components/WidgetSprites.java` | nothing | #393 |
| `AbstractWidget.WithInactiveMessage`, grey `-6250336` | `gui/components/AbstractWidget.java` | nothing | #393 |
| `LinearLayout`, `GridLayout`, `FrameLayout`, `HeaderAndFooterLayout` | `gui/layouts/` | nothing — per-screen arithmetic | #394 |
| `LayoutSettings`, `LayoutElement`, `AbstractLayout`, `SpacerElement` | `gui/layouts/` | nothing | #394 |
| `Screen` focus, `ComponentPath`, `FocusNavigationEvent`, `TabOrderedElement` | `screens/Screen.java` | nothing | #395 |
| `addRenderableWidget` / `addWidget` / `addRenderableOnly` | `screens/Screen.java` | nothing | #395 |
| `EditBox` | `gui/components/EditBox.java` | chat only — `chat.rs`'s `ChatInput`, not a widget | #395 |
| `AbstractSelectionList`, `ObjectSelectionList` | `gui/components/` | nothing reusable | #396, #397 |
| `Tooltip`, `WidgetTooltipHolder` | `gui/components/` | nothing | #393 |
| `GuiSpriteScaling` `Stretch`/`Tile`/`NineSlice{border}` | assets `.mcmeta` | **done** — `lodestone-assets/src/gui.rs`'s `GuiScaling` enum | — |
| nine-slice decomposition | vanilla `blitNineSlicedSprite` | **done** — `GuiScaling::geometry`, `GuiAtlas::geometry` | — |
| GUI sprite atlas | `GuiSpriteManager` | **done** — `GuiAtlas::build`, 466 sprites | — |
| `Renderable.extractRenderState` → `GuiRenderState` | `gui/`, `renderer/state/gui/` | **done in spirit** — `ExtractSet`/`FrameSet` | — |
| `JoinMultiplayerScreen` + `ServerSelectionList` + `ServerStatusPinger` | `screens/multiplayer/` | **partly** — `menu/servers.rs`, `menu/status.rs` | #396 |
| `SelectWorldScreen` + `WorldSelectionList` | `screens/worldselection/` | **nothing** | #397 |
| `OptionsScreen` tree, `OptionInstance`, `OptionsList` | `screens/options/`, `client/` | **all 13 of 13 screens built** (Language, Telemetry, Resource Packs are the last three) — `menu/options.rs`, 143 `OptionsList` controls, 28-or-29 live depending on `in_world` (7 of 93 options persisted, up from 2 previously; see below) — plus Key Binds, Language, Telemetry and Resource Packs (each their own count, see [`language-screen.md`](./language-screen.md)/[`telemetry-screen.md`](./telemetry-screen.md)/[`resource-packs-screen.md`](./resource-packs-screen.md)), none an `OptionsList` page so none is part of the 143 | #55 |
| `AbstractContainerScreen` | `screens/inventory/` | bespoke per screen | #398 |

## What is already done, and must not be re-filed

Sprite/nine-slice reachability was the obvious first work item. It is not one — the mechanism is
complete **and reaches pixels**:

- `crates/lodestone-assets/src/gui.rs`'s `GuiScaling` enum parses `GuiScaling::{Stretch, Tile, NineSlice { border: Border }}`
  from the sibling `.mcmeta`; `GuiScaling::geometry` decomposes to quads.
- `crates/lodestone-render/src/gui_atlas.rs`'s `GuiAtlas::geometry(id, x, y, w, h)` applies it.
- `GuiAtlas::build` stitches **466** `gui/sprites/**` PNGs. Verified against the jar itself:
  `unzip -l .cache/mc/26.2/client.jar | /usr/bin/grep -c 'gui/sprites/.*\.png$'` = 466.
- It is reachable through exactly **two** production call sites —
  `crates/lodestone-render/src/gui_atlas.rs::GuiAtlas::geometry` (menu side in `menu/render.rs::draw`,
  hud side in `hud.rs`). Every other `.geometry(` caller is a test.
- An early startup log once said `sprites=468`. That resolves exactly: 466 + the two `TITLE_TEXTURES`
  extras in `resources.rs`.

So the residue is **breadth of use**, not plumbing: nothing widget-shaped consumes the helper, so
every screen writes its own blits. That is a widget problem, not a sprite issue.

### The one genuinely unreachable set — and it is correct

`GuiAtlas::build` globs `gui/sprites/**` and **structurally cannot see** the **89** GUI textures
outside it: every `container/*.png` background, `advancements/window.png`, `book.png`, the
hanging-sign sheets. `resources.rs`'s `load_gui_atlas` doc comment already documents this and
`container.rs::background::ContainerBackground` already
works around it deliberately — vanilla blits hand-placed sub-rects of those 256×256 sheets at native
size (`ContainerScreen.extractBackground` draws the chest as *two* blits), and `GuiScaling` has no variant
for an arbitrary sub-rect.

**Do not "fix" this by widening the glob.** Forcing the three-mode abstraction to express a sub-rect
blit is the wrong shape, and the workaround is the considered answer.

## The disabled path

The requirement — every real button present, unsupported ones greyed out — maps onto one small
vanilla mechanism. There is **no disabled widget type**; `active = false` is the whole API.

- **Sprite**: `WidgetSprites.get(enabled, focused)` is a 4-state record — `enabled`, `disabled`,
  `enabledFocused`, `disabledFocused`.
- **Label**: `AbstractWidget.WithInactiveMessage` swaps the message to grey **`-6250336`**.
- **No disabled sprite at all** for `Checkbox`, `EditBox`, `AbstractSliderButton` — grey label plus
  blocked input only. Inventing disabled art for these is a deviation that reads as correct in
  review.

Vanilla disables its own controls for exactly our reason, so these are patterns to copy rather than
design:

| site | what and why |
|---|---|
| `OptionsSubScreen.addContents` | narrator, `active = minecraft.getNarrator().isActive()` |
| `VideoSettingsScreen.tick` | anisotropy slider, `active = textureFiltering == ANISOTROPIC` |
| `TitleScreen.createNormalMenuOptions` | multiplayer, `.active = multiplayerAllowed`, with an explaining tooltip |
| `OptionsScreen.init` | telemetry, plus `TELEMETRY_DISABLED_TOOLTIP` |

The last two are the important precedent: **a disabled button carrying a tooltip that says why** is
vanilla's own idiom. It is what makes "unsupported, greyed out" read as honest rather than broken.

Measured detail worth keeping: **`button_disabled`'s nine-slice border width is 1**,
unlike its siblings. Read it from the `.mcmeta` at runtime; do not encode any of them.

## Layout: real, widely used, and not universal

`net/minecraft/client/gui/layouts/` — `AbstractLayout`, `CommonLayouts`, `EqualSpacingLayout`,
`FrameLayout`, `GridLayout`, `HeaderAndFooterLayout`, `Layout`, `LayoutElement`, `LayoutSettings`,
`LinearLayout`, `SpacerElement`.

Two-phase: add children with per-cell `LayoutSettings` (padding + alignment), then one
`arrangeElements()` pass assigns absolute bounds, then `visitWidgets` hands them to the screen.
`OptionsSubScreen.init` is the canonical shape:

```java
this.addTitle();
this.addContents();
this.addFooter();
this.layout.visitWidgets(x$0 -> this.addRenderableWidget(x$0));
this.repositionElements();     // -> this.layout.arrangeElements()
```

Usage under `client/gui/screens/` — **57 files reference a layout class, 47 of them `Screen`
subclasses** (68 files across the whole client tree):

| class | files under `screens/` |
|---|---|
| `LinearLayout` | 46 |
| `HeaderAndFooterLayout` | 27 |
| `FrameLayout` | 17 |
| `GridLayout` | 10 |
| `EqualSpacingLayout` | 1 |

`HeaderAndFooterLayout` matters most: it is the base of `OptionsSubScreen` (its `layout` field), so **every**
settings sub-screen inherits it, and both target list screens use it. Landing order by leverage:
`LinearLayout` → `HeaderAndFooterLayout` → `FrameLayout` → `GridLayout`; skip `EqualSpacingLayout`
until something needs it.

Two counts bound it in the other direction:

- **`screens/inventory/` uses it zero times, across all 59 files.** `AbstractContainerScreen.init`
  hand-centres via `leftPos`/`topPos`; slot positions come from the *menu* classes as
  constructor arithmetic — `ChestMenu.addChestGrid`,
  `this.addSlot(new Slot(container, x + y * 9, left + x * 18, top + y * 18))` — which is shared
  server/client code, not client UI code.
- **`TitleScreen` uses zero layout classes**, hand-centring on `this.width / 2 - 100`.

So "vanilla uses layouts" is not universal, and a hand-arithmetic screen is legitimate vanilla.

## The settings census

Vanilla models an option as an `OptionInstance` (`client/OptionInstance.java`, 644 lines).
`Options.java` declares **94 private `OptionInstance` fields** with **93 public accessors**.
`ValueSet` implementations: `Enum`, `LazyEnum`, `AltEnum`, `IntRange`,
`ClampingLazyMaxIntRange`, `UnitDouble`, `SliderableEnum`, over the
`CycleableValueSet` / `SliderableValueSet` / `SliderableOrCyclableValueSet`
interfaces. `createButton` dispatches to a `CycleButton` or an `OptionInstance.OptionInstanceSliderButton`.

Every sub-screen extends `OptionsSubScreen` = `HeaderAndFooterLayout` + `OptionsList` +
abstract `addOptions()` + Done footer. `OptionsList` offers `addBig` (full-width row), `addSmall`
(two per row), `addHeader`. **The census is therefore the `addBig`/`addSmall` call sites** — a
settings screen is a list of options, not bespoke geometry.

| screen | controls | notes |
|---|---|---|
| `OptionsScreen` (root) | 15 | FOV; World Options*; Online*; 9 nav buttons; Credits; Done |
| `VideoSettingsScreen` | 31 | 3 headers; fullscreen-resolution built inline in `addOptions`; display 8 in `displayOptions`; quality `graphicsPreset` + 17 in `qualityOptions`; preferences 4 in `preferenceOptions` |
| `KeyBindsScreen` | 59 | 57 `KeyMapping`s over 8 categories, + Reset All + Done |
| `AccessibilityOptionsScreen` | 25 | 24 options in `options` + Controls link in `addOptions`; footer help link in `addFooter` |
| `ChatOptionsScreen` | 18 | its `options` method |
| `SoundOptionsScreen` | 16 | master + 10 other `SoundSource`s, `soundDevice`, subtitles, directional, `musicFrequency`, `musicToast`, all in `addOptions` |
| `ControlsScreen` | 9 | 2 nav + 7 toggles in `options` |
| `SkinCustomizationScreen` | 8 | 7 `PlayerModelPart`s + `mainHand` in `addOptions` |
| `MouseSettingsScreen` | 7 | 6 in `options` + `rawMouseInput`* |
| `OnlineOptionsScreen` | 7 | **built** — `SettingsPage::Online`, all seven decorative. 3 headers; friends, requests, notifications, presence, Xbox link, `allowServerListing`, `realmsNotifications` |
| `FontOptionsScreen` | 2 | `forceUnicodeFont`, `japaneseGlyphVariants` in `options` — present-and-inactive on the built `LanguageSelectScreen`'s footer, not yet its own page |
| `LanguageSelectScreen` | list | **built** — `SettingsPage::Language`, the third list-widget kind (`ObjectSelectionList`); see [`language-screen.md`](./language-screen.md) |
| `TelemetryInfoScreen` | info | **built** — `SettingsPage::Telemetry`, an honest prose screen (no event log, no opt-in state — this client collects no telemetry); see [`telemetry-screen.md`](./telemetry-screen.md) |
| `PackSelectionScreen` | list | **built, deliberately reduced** — `SettingsPage::ResourcePacks`: one always-empty list, one always-one-entry list, no drag-between transfer controls (nothing for them to do); see [`resource-packs-screen.md`](./resource-packs-screen.md) |

**~198 distinct interactive controls**, of which 57 are keybind rows — **141 excluding keybinds**.
Counted as focusable/clickable widgets, excluding per-screen Done/Cancel except where noted; headers
not counted. Numbers marked `*` are conditional at runtime.

Options the informal earlier list omitted, worth naming so they are not discovered late:
`prioritizeChunkUpdates`, `cloudRange`, `cutoutLeaves`, `improvedTransparency`, `textureFiltering`,
`maxAnisotropyBit`, `weatherRadius`, `inactivityFpsLimit`, `exclusiveFullscreen`,
`preferredGraphicsBackend`, `chunkSectionFadeInTime`, `vignette`, `saveChatDrafts`,
`onlyShowSecureChat`, `hideMatchedNames`, `reducedDebugInfo`, `musicFrequency`, `musicToast`,
`allowCursorChanges`, `rotateWithMinecart`, `highContrastBlockOutline`, `narratorHotkey`,
`hideSplashTexts`, `notificationDisplayTime`, `backgroundForChatOnly`, `sprintWindow`,
`toggleAttack`, `toggleUse`, `japaneseGlyphVariants`.

### What we persist

**Two of 93, not four — this section said four and was wrong.** The error is
`CLAUDE.md` rule 2's shape exactly: it was produced by counting `config.rs`'s public fields, and
`config.rs` holds **two** structs.

- `config::Options` is the *persisted* one (`options.json`) and has three fields: `gui_scale`,
  `keybinds`, `view_bobbing`. Only two of those are vanilla `OptionInstance`s.
- `config::Config` is *argv*, and its own doc comment says it is "parsed fresh from argv every run
  and never written back". `render_distance` and `sensitivity` live **there**, so a settings row that
  appeared to set either would be fabricated persistence — honoured for the session by accident of
  the CLI default and lost on restart.

So the live pair is `gui_scale` and `view_bobbing`, and the settings tree renders `renderDistance` and
`sensitivity` inactive with the rest. Making them live is real work: a field on `Options`, a JSON
key, and a consumer in `app.rs` that prefers it over the flag — and `sensitivity` additionally
cannot be an `f32` without `Options` losing its `Eq`.

**2 of 93 at first landing, 7 of 93 now.** That ratio is the argument for building the tree from an
option *model* rather than screen by screen: most rows will be present-and-disabled for a long time,
and that is the intended end state, not a shortfall. The settings tree first landed 135 controls of
which 18 worked; a later pass made five more Controls/Mouse rows live without changing the
135-control census, so 23 worked after that pass. The Online settings page (`task_036bd7b9`) then added a ninth page —
`OnlineOptionsScreen` above, all seven of its own controls decorative — bringing the census to
**143** and, for the first time, making the live count context-dependent: the root's Online button
is a tenth live row outside a world (**25**) and stays the inactive World Options placeholder inside
one (**24**), because `WorldOptionsScreen` itself is still not built. See
[`settings-screen.md`](./settings-screen.md) for the exact split and the tests that hold both
numbers.

## What already exists on our side

Re-verified rather than assumed — `CLAUDE.md` rule 2, and it changed the plan twice.

- **The server list is not absent.** `Screen::ServerList` and `Screen::ServerEdit` are live variants
  of the `menu.rs` `Screen` enum, and `render.rs` references `Screen::ServerList` in 5 places. `menu/servers.rs`
  (528 lines) has `ServerEntry`/`ServerList` with `split_host_port`, `effective_port`, JSON
  persistence and `servers_path()`. `menu/status.rs` (470 lines) has `StatusCache` with a real
  `net_probe`, `pump()` and pending states. This is a **fidelity pass**, not a build.
- **World select is genuinely absent.**
  `/usr/bin/grep -ric 'worldselect\|world_select\|WorldList\|LevelSummary'` over
  `crates/lodestone-shell/src/` returns no non-zero count, and the `Screen` enum has no variant.
  It is the only target screen that is new construction.
- **The settings tree already had an umbrella**, with a keybinds screen and other work beneath it.
  It was not re-filed; the census above was added there as a comment.
- `crates/lodestone-shell/tests/menu_button_pixels.rs` is the established GUI pixel-gate pattern and
  the model for every new gate here.
- Text entry exists, but it is **chat's** — `chat.rs`'s `ChatInput`, rendered in `hud.rs`.
  This is a trap to watch: do not disturb it while adding `EditBox`.

## The boundary: what not to build

This framework is for **menu screens**. Two neighbours are explicitly out.

**The HUD.** Driven by live game state every frame, with its own layout logic in `hud.rs`, anchored
to a *moving* cluster origin that already burned one gate (`CLAUDE.md`'s `sprite_vitals` entry: a
gate measured ~20 logical pixels above a row that was drawing perfectly). It is not a widget tree and
must not be retrofitted into one. Hearts, hunger, bubbles, XP, hotbar, chat overlay, boss bars, and
the F3 overlay are all out.

**Container screens.** Driven by live `Menu` state, with slot geometry inherited from the *menu*
classes rather than any layout container — the 0-of-59 count above is the evidence. They get their
own pass, last.

What crosses the boundary and what does not:

- **Primitives cross**: the font stack, `GuiAtlas`, the nine-slice helper, and any widget the
  container pass wants to reuse.
- **Layout does not**: a container screen must never be arranged by `HeaderAndFooterLayout`, because
  vanilla does not do that and copying it would invent geometry vanilla never had.

Also out: world **creation** (the target screen renders the button disabled and stops), Realms, and the
account screens, which are ours rather than vanilla's.

## Sequencing, and why it changed

The plan started as: sprite/nine-slice reachability → layout → widgets → `Screen` dispatch → the
three screens → `AbstractContainerScreen`. Two changes, both from measurement:

1. **Sprite reachability is deleted, not scheduled.** It is already done and already reaching pixels
   (above). What looked like a plumbing gap is a breadth-of-use gap, which is the widget issue.
2. **Widgets move ahead of layout.** Vanilla's `AbstractLayout` arranges `LayoutElement`s — there is
   nothing to arrange until a widget type exists. And `TitleScreen` proves a widget is perfectly
   usable with hand-placed bounds in the meantime, so nothing is blocked by deferring layout. The
   disabled path is also the immediate requirement and the smallest piece, which makes it the right
   first landing on its own.

| # | issue | what consumes it |
|---|---|---|
| 1 | #393 widgets + disabled path | converting the existing title and pause button rows |
| 2 | #394 layout primitives | re-expressing title, pause, settings, server list — pixels unmoved |
| 3 | #395 focus, tab, dispatch, `EditBox` | converting `Screen::ServerEdit`'s address fields |
| 4 | #396 server list fidelity | already reachable from the title screen |
| 5 | #397 world select, creation disabled | wiring the title screen's Singleplayer button, same PR |
| 6 | #55 settings tree (+#15, #32, #195) | the Options button on title and pause |
| 7 | #398 `AbstractContainerScreen` | refactoring inventory + chest onto it; absorbs the slot-highlight work |

## How to change this

- **Every child issue must name its consumer.** The dominant defect here is the island — sixteen
  confirmed, most recently a view bob whose mechanism landed with a passing pixel gate while its
  consumer sat uncommitted for two hours. A widget type with no converted screen is an island;
  reject it on that basis rather than merging and following up.
- **Layout gates assert widget rects against vanilla's computed positions**, not screenshots.
  Hand-derive the expected `(x, y, w, h)` from `arrangeElements`' own lines, or dump from a JVM
  oracle. Per `CLAUDE.md`, the expected value must originate **outside the code under test** — do not
  snapshot our own output as a fixture, because `decode(encode(x)) == x` is satisfied by two
  symmetric misunderstandings.
- **Predict the value, do not assert the sign.** The disabled label must land on `-6250336`
  specifically, not merely "darker than enabled". That is the *magnitude* species of vacuous test:
  the hurt-overlay gate asserted direction and passed at 3440/3440 while rendering ~70% red where
  vanilla renders ~30%.
- **Report a bounding box, never a percentage.** A gate reporting only a fraction cannot tell a
  uniform-but-wrong frame from a localised blob.
- **Derive geometry from the same expression the draw uses.** Ask the widget or the layout for its
  bounds; do not restate a constant.
- **Every absence assertion needs a control watched failing** — and before believing the control,
  ask *what else already paints here*. The precedent in this exact area is
  `container_screen.rs`'s "nothing else draws at the test cursor position", which broke when the dim
  gradient landed; the right fix was a `skip_verts` parameter passed `dim_vertex_count`, restoring
  the question the control asked rather than loosening the assertion.
- **For a hover or highlight, the discriminator is *position*.** Move the cursor and assert the
  thing moves with it. A gate proving "something drew in a slot" passes on a highlight nailed to
  slot 0.
- **Use `/usr/bin/grep`, never `rtk grep`,** for any claim in this document. It strips the matched
  pattern and everything before it on the line, so a symbol that exists reads as absent — and this
  document is mostly absence claims. A truncated search is not a negative result either: use
  `grep -c` or narrow the path.

## Configuration

- `crates/lodestone-shell/src/config.rs` — the persisted `Options` (`options.json`). **Two** vanilla
  options today (`gui_scale`, `view_bobbing`) plus the `keybinds` map, which is its own separate
  concern rather than an option row. `render_distance` and `sensitivity` are *not* here — they live
  on `config::Config`, which is argv-only and never written back — so the settings tree renders them
  inactive. See the census under [What we persist](#what-we-persist); the "four" this line used to
  claim came from counting both structs.
- `crates/lodestone-shell/src/resources.rs` — `load_gui_atlas()` (HUD) and `load_menu_gui_atlas()`
  (menu, `build_with_extras` + `TITLE_TEXTURES`). Two stitches deliberately; `load_menu_gui_atlas`'s
  own doc comment explains why, and notes the tidier end state is one shared atlas handed to both renderers.

## Dependencies

- `lodestone-assets` — `gui.rs`: `.mcmeta` parsing, `GuiScaling`, `Border`, quad decomposition.
- `lodestone-render` — `gui_atlas.rs`: `GuiAtlas`, `GuiSpriteQuad`, `geometry()`.
- `lodestone-shell` — `menu/{render,nav,servers,status,accounts}.rs`, `menu.rs`, `hud/{font,vanilla_font,item_icon}.rs`,
  `container.rs`, `config.rs`.
- The 26.2 jar at `.cache/mc/26.2/{client-src,client.jar}` — behavioural reference only, never
  transliterated.

## See also

- [Main menu](./main-menu.md) — the screen state machine, the persisted server list, status pings.
- [Pause menu](./pause-menu.md) — the Escape stack and why `Screen::Paused` stays out of `owns_frame`.
- [Container screen](./container-screen.md), [Container clicks](./container-clicks.md),
  [GUI item icons](./gui-item-icons.md) — the container side, and the two-run colour stream the
  `AbstractContainerScreen` port must preserve.
- [Keybindings](./keybindings.md) — the action → input table, and why Escape is not rebindable.
