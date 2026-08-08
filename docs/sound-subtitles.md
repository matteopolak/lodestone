# Sound subtitle captions

## What it is

Vanilla's accessibility overlay that names each sound as it plays — a stack of
right-aligned plates just above the hotbar, oldest at the bottom, each fading from
white to grey over three seconds, with a `<` or `>` arrow when the sound came from
behind you. Gated by the `showSubtitles` option (issue #198).

## How it works

Four pieces, in order.

**1. The data.** `SoundEvent.subtitle` (`crates/lodestone-assets/src/sound.rs`) has
always been parsed out of the real `sounds.json`; it just had no consumer.
`SoundResolver::subtitle` / `AudioEngine::subtitle` expose it by event name.

That accessor reads the event **before** weighted selection, deliberately.
`SoundRegistry::resolve` consumes an RNG roll to pick an entry, and the subtitle is
a property of the *event*, not of the chosen entry — going through `resolve` would
both waste a roll and desync the seeded selection every other client agrees on.

**2. The hook.** `ShellAudio::play_sound` and `play_entity_sound`
(`crates/lodestone-shell/src/audio.rs`). These two are the single choke point every
sound in the client passes through — network sounds, entity sounds, local
block-place prediction, ambience, footsteps — so recording the caption *there* is
what makes captions structurally unable to disagree with what is audible. Do not
add a second detection mechanism at a caller; that is exactly how they would drift.

It records **before** the engine call, not after: a resolve failure (a missing
`.ogg` in an incomplete corpus) still means the event fired, and vanilla's own
`SoundEventListener` hook likewise runs off submission rather than off successful
decode.

**3. The queue.** `crate::audio::subtitles::SubtitleQueue`, a port of
`SubtitleOverlay`'s inner `Subtitle` list. Captions are keyed on their **text**, and
a repeat refreshes the existing row's timestamp and position list instead of
stacking a duplicate — otherwise walking on grass produces a wall of identical
lines. `views` purges the 3-second window and turns the queue plus a listener basis
into `SubtitleCaption { text, brightness, arrow }`.

**4. The draw.** `hud::draw_sound_subtitles`, fed from
`HudFrame::sound_subtitles`, which `app/redraw.rs` fills from
`Sim::sound_subtitles` when `Options::show_subtitles` is on.

## Three things that are easy to get backwards

**It fades brightness, not alpha.** `SubtitleOverlay.java` lerps the text's RGB from
`255` down to `75` and leaves alpha at `255` throughout, with the background plate
at a constant opacity. Fading alpha instead makes an old caption translucent over
the world rather than grey on its own plate — a visibly different effect, and the
one a naive port produces.

**Every plate is the same width.** The width is `max(text widths)` plus the width of
`"<"`, `">"` and two spaces, so the arrow columns exist on every row and a row
without an arrow does not shrink. Sizing each plate to its own text gives a ragged
stack that reads as a layout bug.

**The text is centred inside that width**, even though the plate itself is
right-aligned. Left-aligning the contents looks almost right.

## How to change it, and the gotchas

- **The option is on two pages.** Vanilla places `showSubtitles` on both
  `SoundOptionsScreen` and `AccessibilityOptionsScreen`, and so do we —
  `LiveOption::ShowSubtitles` appears twice in `menu/options.rs`. Both rows drive
  the one `config::Options::show_subtitles` field. Three of the chat options already
  had this shape; the census tests in `menu/options.rs` assert the exact duplicated
  list, so adding or moving a live row means updating them.
- **`Sim::sound_subtitles` needs `&mut self`** (reading the queue purges it) and is
  therefore collected in `app/redraw.rs` *above* `let item_models = …`, which holds
  an immutable borrow of `self.sim` all the way down to the hotbar draw. Moving the
  call down beside the rest of the `HudFrame` assignments does not compile.
- **Translation happens in `Sim`, not in the queue.** `subtitle` is a translation
  key (`subtitles.block.stone.break`); `Sim::sound_subtitles` resolves it against
  the loaded language table and falls back to the raw key, the same degradation
  every other translated string here takes. A jar-less run therefore shows keys
  rather than nothing, which is the honest signal.
- **Range is not modelled.** Vanilla also drops a caption whose sound is further
  away than that sound's own attenuation range (`Subtitle.isAudibleFrom`). The hook
  has the event name and position but not the resolved entry's
  `attenuation_distance`, so every caption here is treated as audible. The sound was
  actually submitted to the mixer, which is a stronger audibility signal than a
  range check, and the 3-second window expires it anyway. Threading the range out of
  `SoundResolver` is the fix if it ever matters.
- **`notificationDisplayTime` is not modelled.** Vanilla multiplies the 3000 ms
  window by it; at its default of `1.0` the product is the constant, so
  `DISPLAY_MS` is correct today and wrong the moment that slider becomes live.

## Configuration

| key | default | effect |
|---|---|---|
| `show_subtitles` (`options.json`) | `false` | the whole overlay; vanilla's own default is off too |

Reachable in-game from Options → Sound → Closed Captions, or Options →
Accessibility → Closed Captions.

## Dependencies

- `lodestone-assets` — `sound::SoundRegistry`/`SoundEvent` for the `subtitles` key,
  and `Language` for the translation table.
- `lodestone-sound` — `SoundResolver::subtitle` / `AudioEngine::subtitle`.
- `lodestone-shell` — `audio::ShellAudio` (the hook), `audio::subtitles` (the
  queue), `sim::audio` (the accessor), `hud` (the draw), `menu::options` +
  `menu::nav` (the toggle), `config` (persistence).
