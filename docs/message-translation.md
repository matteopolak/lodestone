# Message translation

## What it is

How a server-authored `Text` component becomes words on screen: the `en_us.json`
table read out of `client.jar`, the `translate`-node resolver that expands it, and
where command feedback, colours and italics come from. This is the doc to read
before adding a message anywhere, and before assuming a translation table needs
building — most of this already exists.

## How it works

Four pieces, all already wired. The single most useful fact about this subsystem
is that **it is not a gap**: a `translate` component sent by a server today
resolves correctly, with arguments and styling, at every surface that displays it.

| piece | where | what it does |
|---|---|---|
| the table | `lodestone-assets/src/lang.rs` (`Language`) | parses a flat `key -> pattern` JSON object; `Language::get`; `Language::translator()` hands it out as a closure |
| loading it | `lodestone-shell/src/resources.rs` (`try_vanilla`) | reads `assets/minecraft/lang/en_us.json` from the same `client.jar` every texture comes from — **8,123 keys / 519,377 bytes** in 26.2 |
| the seam | `Sim::translator()` → `Translator<'a> = Box<dyn Fn(&str) -> Option<String>>` | one borrowed closure, handed to the pure projection helpers at the read boundary |
| the resolver | `lodestone-game/src/text.rs` (`resolve`, `resolve_to_string`) | walks a `Text` tree replacing every `translate` node with its literal expansion |

The resolver is complete against vanilla's format language: `%s` (sequential),
`%N$s` (1-based indexed), `%%`, **recursively resolved** arguments (an argument may
itself be a `translate` node — that is the normal case), `fallback`, and
key-as-last-resort. Style inheritance is `TextStyle::inherit` in
`lodestone-model/src/text.rs`: for every attribute the child's `Some` wins,
otherwise the parent's value is inherited — the one place inheritance is defined,
and both flattening and formatting go through it. `hud/vanilla_font.rs` really
draws bold, italic, underline, strikethrough and obfuscated, so a style that
resolves reaches pixels.

### Where it is consumed

- **Chat** — `sim/net_apply.rs`'s `NetUpdate::Chat` arm calls `resolve_text` at
  arrival, so the stored scrollback and the log line both read as prose.
- **Title / action bar** — `sim/session.rs`, via `resolve_text(..).to_legacy_string()`.
- **Disconnect reason** — `sim/net_apply.rs`.
- **Scoreboard** — `scoreboard.rs`, via `resolve(..).to_spans()`.
- **Container titles and the "Inventory" label** — `container::menu_title`,
  `player_inventory_title`, `player_inventory_label`.

Two of those flatten through `to_legacy_string()`, which encodes style as `§`
codes. That is lossy for `TextColor::Rgb` only (no legacy code exists — see
`hud.rs`'s note); bold/italic/colour-by-name survive.

### Where vanilla's italics come from

Not from command feedback. `CommandSourceStack.java:495` sends a **failure** as
`Component.empty().append(message).withStyle(ChatFormatting.RED)` — red, upright.
The italics are the **op broadcast**, `CommandSourceStack.java:480`:

```java
Component broadcast = Component.translatable("chat.type.admin", this.getDisplayName(), message)
    .withStyle(ChatFormatting.GRAY, ChatFormatting.ITALIC);
```

`chat.type.admin` is `"[%s: %s]"`, so an op sees
`[Server: Set own game mode to Creative Mode]` in grey italic, with the nested
feedback message **inheriting** that style. A resolver that reset style per
substituted argument would leave the inner sentence upright while the brackets
stayed italic — which reads as "nearly right" on screen and is the reason
`tests/command_message_translation.rs` asserts on every span rather than on the
root.

### Command feedback: what the server sends today

`lodestone-server/src/commands/*.rs` call `ctx.send_success(format!(…))` and
`Registrar`'s `feedback` field is a `Vec<String>`, so nothing on the wire is a
component yet. The wording is close but not vanilla's, and the arguments differ in
kind. `tests/command_message_translation.rs` pins the target; the table in that
file's header records which decompiled line each key and argument order came from.

## How to change it, and the gotchas

- **Do not add a second translation mechanism.** `Sim::translator()` is the seam;
  a helper that takes `&dyn Fn(&str) -> Option<String>` composes with everything
  above. `container::menu_title` is the shortest example to copy.
- **Do not bundle `en_us.json` as a committed dump.** Weighed and declined: it is
  519 KB / 8,123 keys of data the repo *already requires* (`client.jar` is a hard
  dependency of every texture and the font), a committed copy would need its own
  drift gate against the jar, and it can only ever be *en_us* — so it buys nothing
  toward multiple languages. The runtime path already degrades honestly: no table
  → `None` → the component's `fallback` → the raw key.
- **A missing key must show the key.** That is vanilla's behaviour and the right
  one: it makes the miss visible instead of silently blank. Do not "improve" it
  into an empty string.
- **`%s` and `%1$s` both occur in the real table** and mixing them within one
  pattern is legal. Do not assume one form.
- **An argument is usually a component, not a string.** `/give`'s middle argument
  is the item's `getDisplayName()` (→ "Diamond Sword"), not its id
  (`minecraft:diamond_sword`); `/gamemode`'s is
  `translatable("gameMode." + name)` (→ "Creative Mode"), not the mode's name.
  Interpolating the id or the enum name is the specific mistake the current
  literal strings make.
- **Verify a key against the jar, never from memory.** `PATTERNS` in
  `tests/command_message_translation.rs` is transcribed by hand *on purpose* — the
  always-on tests expand it by hand so the expected sentence originates outside
  the resolver, and the `#[ignore]`d `transcribed_patterns_match_the_real_en_us_json`
  is the drift gate that keeps the transcription honest. Feeding the jar's pattern
  to the resolver and asserting the resolver agrees with the jar's pattern is
  `decode(encode(x)) == x`.
- **26.2's `GiveCommand` uses `commands.give.success.single` in *both* branches**
  (`GiveCommand.java:98` and `:102`), passing `players.size()` as the third
  argument in the multi-target case — so the multi-target line reads "Gave 3
  Diamond Sword to 2". `commands.give.success.multiple` ("Gave %s %s to %s
  players") exists in `en_us.json` and is unused there. That is what the record
  says; transcribe it, do not correct it silently.

## Languages other than English

`en_us` is the extractable one and **every other language is not in the jar**.
Measured against 26.2's `asset-index-32.json`:

| fact | value |
|---|---|
| `minecraft/lang/*.json` objects in the asset index | **142** |
| total size | **86.6 MB** (median 580 KB each) |
| present in `.cache/mc/26.2/objects` today | **0** |
| `minecraft/lang/en_us.json` in the index | **no** — en_us is jar-only |
| the language *manifest* | `pack.mcmeta`, **19,652 bytes**, index-only (the jar has no `pack.mcmeta` at all) |

`LanguageManager.extractLanguages` (`.cache/mc/26.2/client-src/.../language/LanguageManager.java:33`)
reads each resource pack's `LanguageMetadataSection` — i.e. `pack.mcmeta`'s
`"language"` object, code → `{name, region, bidirectional}`. That corrects
`docs/language-screen.md`'s note that the display names are "public vanilla
knowledge, not jar-verified": there is no `languages.json` in 26.2, and the real
manifest **is** obtainable, as one 19.6 KB object.

So the remaining work is a fetch and a selection, not a parser:

1. `pack.mcmeta` and the chosen `minecraft/lang/<code>.json` are asset-store
   objects. `lodestone-shell/src/asset_objects.rs` already reads that store
   (hash-addressed, `objects/<hash[0..2]>/<hash>`) and `xtask`'s
   `download_verified_file` / `fetch-sounds` already do verified downloads of
   index objects — so neither the reader nor the downloader is new.
2. Fetch **`pack.mcmeta` eagerly** (19.6 KB) to populate the picker, and each
   `lang/<code>.json` **lazily on selection** (~580 KB). Fetching all 142 up front
   is 86.6 MB for one language's worth of use.
3. `menu/language.rs`'s `LANGUAGES` becomes the manifest's entries instead of the
   single hardcoded `en_us`, and its "decorative — the selection's effect" note
   becomes real: selection writes the code to config and reloads `Sim`'s table.
4. `resources.rs` loads `Language` from the store object for the selected code,
   falling back to the jar's `en_us` — the fallback matters, because a language
   whose object has not been fetched must degrade to English rather than to keys.

`assets/minecraft/lang/deprecated.json` (27,841 B, in the jar) is vanilla's
old-key → new-key alias map. Not needed for en_us; it is how vanilla keeps a
stale key working, so it belongs with this work rather than before it.

## Configuration

- `LODESTONE_ASSETS` — pack root holding `client.jar`; otherwise the highest-sorting
  `.cache/mc/<ver>` found by walking up from the working directory
  (`resources::asset_root`).
- No env var or flag selects a language today; `en_us` is the only table loaded.

## Dependencies

- `lodestone-assets` — `Language`, `ZipSource`, `ResourceManager`.
- `lodestone-model` — `Text`, `TextContent::Translate`, `TextStyle::inherit`,
  `TextColor`, and the tiny built-in stub table (`default_translation`, fourteen
  chat/death keys) that applies when no real table is loaded.
- `lodestone-game` — `text::resolve` / `resolve_to_string`.
- `client.jar` for `en_us.json`; the launcher asset-object store for anything else.
