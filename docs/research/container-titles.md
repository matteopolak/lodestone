# Diagnosis: container screen titles (names, capitalisation, placement, named containers)

## What it is

Three of the four reported container-title problems are already fixed and pixel-gated: the
wire string, its capitalisation, and named-container resolution all trace correctly through
the live language table and both event routers. The one real, still-open gap is placement —
`label_layout` only models vanilla's `titleLabelX`/`titleLabelY` anchor for 3 of its ~20
container screens, so 10 more (furnace family, brewing stand, anvil, loom, stonecutter,
cartography table, dispenser/dropper, crafter) draw their title at the wrong default `(8, 6)`
anchor. §5 below is a ready-to-apply, additive patch spec for exactly that gap.

**Read-only investigation. No repo files were edited.** Everything below is either a confirmed
existing behaviour (with evidence) or a patch specification for something genuinely missing.

## Headline finding

Three of the four claims in the report are **already fixed and pixel-gated** in the current
committed tree (`crates/lodestone-shell/src/container.rs`, commit `812eb67` "fix(container):
vanilla's two screen labels — right words, typeface, anchors (#370)", plus `76fff73`/issue #52 for
the language-table resolution). The player report reads like it predates that fix, or was tested
against a build before it landed — `git status` at the start of this task shows `container.rs`,
`app.rs` and `sim.rs` are **not** among the files other agents currently have modified, so what
follows is the real, current, on-disk behaviour, not a stale snapshot.

What is **not** fixed, and is real: vanilla's per-screen title anchor (`titleLabelX`/`titleLabelY`)
is only modelled for 3 of vanilla's ~20 container screens (`Player`, the crafting table, and the
generic `(8,6)` default). Ten more real screens — furnace family, brewing stand, anvil, loom,
stonecutter, cartography table, dispenser/dropper, crafter — draw their title at vanilla's wrong
anchor today. `label_layout`'s own doc comment already flags this as "Not modelled." Section 3 below
is a ready-to-apply, additive fix for exactly that gap.

---

## 1–2. The string and its capitalisation

Vanilla never sends the words "Crafting"; `ClientboundOpenScreenPacket` carries a `Component`
(`.cache/mc/26.2/src/net/minecraft/network/protocol/game/ClientboundOpenScreenPacket.java:24`), and
the **server** decides whether that component is the container's default translatable key or a
custom literal name — see §4. The default key comes from each container block entity's
`getDefaultName()` (`BaseContainerBlockEntity.java:66`), one `DEFAULT_NAME` constant per type:

| container | wire `menu_type` | `container.*` key | `en_us.json` string | block entity source |
|---|---|---|---|---|
| Chest | `minecraft:generic_9x3`/`9x6` | `container.chest` | "Chest" | `ChestBlockEntity.java:30` |
| Ender Chest | `minecraft:generic_9x3` | `container.enderchest` | "Ender Chest" | (client-only menu, `EnderChestBlock.java`) |
| Barrel | `minecraft:generic_9x3` | `container.barrel` | "Barrel" | (`BarrelBlockEntity`, not separately checked) |
| Shulker Box | `minecraft:shulker_box` | `container.shulkerBox` | "Shulker Box" | `ShulkerBoxBlockEntity.java:44` |
| Crafting Table | `minecraft:crafting` | `container.crafting` | "Crafting" | `CraftingTableBlock.java` |
| Player inventory (E) | *(no packet — local screen)* | `container.crafting` | "Crafting" | `InventoryScreen.java:28` |
| Furnace | `minecraft:furnace` | `container.furnace` | "Furnace" | `FurnaceBlockEntity.java:12` |
| Blast Furnace | `minecraft:blast_furnace` | `container.blast_furnace` | "Blast Furnace" | `BlastFurnaceBlockEntity.java:13` |
| Smoker | `minecraft:smoker` | `container.smoker` | "Smoker" | `SmokerBlockEntity.java:13` |
| Brewing Stand | `minecraft:brewing_stand` | `container.brewing` | "Brewing Stand" | `BrewingStandBlockEntity.java:40` |
| Dispenser | `minecraft:generic_3x3` | `container.dispenser` | "Dispenser" | `DispenserBlockEntity.java:18` |
| Dropper | `minecraft:generic_3x3` | `container.dropper` | "Dropper" | `DropperBlockEntity.java:8` |
| Hopper | `minecraft:hopper` | `container.hopper` | "Item Hopper" | `HopperBlockEntity.java:36` |
| Crafter | `minecraft:crafter_3x3` | `container.crafter` | "Crafter" | `CrafterBlockEntity.java:35` |
| Anvil | `minecraft:anvil` | `container.repair` | "Repair & Name" | (`AnvilMenu`, server-built title) |
| Grindstone | `minecraft:grindstone` | `container.grindstone_title` | "Repair & Disenchant" | (`GrindstoneMenu`) |
| Enchanting Table | `minecraft:enchantment` | `container.enchant` | "Enchant" | `EnchantingTableBlockEntity.java:20` |
| Loom | `minecraft:loom` | `container.loom` | "Loom" | (`LoomBlock.java`) |
| Stonecutter | `minecraft:stonecutter` | `container.stonecutter` | "Stonecutter" | (`StonecutterBlock.java`) |
| Cartography Table | `minecraft:cartography_table` | `container.cartography_table` | "Cartography Table" | (`CartographyTableBlock.java`) |
| Beacon | `minecraft:beacon` | `container.beacon` | "Beacon" | `BeaconBlockEntity.java:67` |
| Merchant (villager) | `minecraft:merchant` | *(no static key — `MerchantScreen` composes `merchant.title` + level)* | — | out of scope, see §5 |
| Player's own second label | *(client-side, not from a packet)* | `container.inventory` | "Inventory" | `Inventory.java:55` |

`en_us.json` line numbers (`.cache/mc/26.2/src/assets/minecraft/lang/en_us.json`): `container.barrel`
3193, `blast_furnace` 3197, `brewing` 3198, `cartography_table` 3199, `chest` 3200, `crafter` 3202,
`crafting` 3203, `dispenser` 3205, `dropper` 3206, `enchant` 3207, `enderchest` 3214, `furnace` 3215,
`grindstone_title` 3216, `hopper` 3217, `inventory` 3218, `loom` 3221, `repair` 3222, `shulkerBox`
3225, `smoker` 3229, `stonecutter` 3231.

**Where our string comes from.** `decode_open_screen`
(`crates/protocol/v770/src/adapter.rs:1376-1390`) decodes the wire NBT title into a
`lodestone_model::Text` (`Text::from_nbt`, preserving the `translate`/`literal` distinction — it
does **not** flatten early). That `Text` rides `ClientEvent::ScreenOpened` through
`Menus::apply` (`crates/lodestone-game/src/menus.rs:224-236`) to `Menus::opened_title()`
(`menus.rs:199`), and is flattened to a string exactly once, at the shell's read boundary:
`crates/lodestone-shell/src/app.rs:2257` calls
`crate::container::menu_title(&open.title, self.sim.translator().as_ref())`, which is
`lodestone_game::text::resolve_to_string` (`container.rs:243-248`) — i.e. resolved through the
**live language table** (`Language::translator`), not the model crate's own 14-key stub
(`lodestone_model::Text::to_plain_string`'s doc comment at `crates/lodestone-model/src/text.rs:338-364`
names this exact defect and cites this exact fix by number: "the container-screen title did not
[go through the language table], and shipped `container.crafting` where 'Crafting' belonged").

The player inventory screen (opened with `E`, no packet involved) gets the same treatment via
`container::player_inventory_title` (`container.rs:266-272`), which resolves
`Text::translate("container.crafting", vec![])` — matching `InventoryScreen.java:28` exactly,
including the non-obvious fact that the **player inventory screen's title is "Crafting," not
"Inventory"** (that word is the *second* label, drawn separately).

**Capitalisation**: since the string is the live language table's own value, there is no separate
uppercasing step to get wrong — except there used to be one. `container.rs:728-731`'s comment
records the exact prior bug: the title used to go through `.to_ascii_uppercase()`, so a chest
renamed "Loot" drew as "LOOT". That call is gone; confirmed by reading the current `menu_title`/
`player_inventory_title` functions (`container.rs:224-272`), neither of which touches case.

**Conclusion: nothing to patch for claims 1–2.** They are correct, sourced from the real language
table, and pixel-gated (`crates/lodestone-shell/tests/container_labels.rs::a_custom_name_reaches_the_panel_verbatim_and_nothing_is_uppercased`,
line 498).

---

## 3. Placement and font

### Font, colour, shadow — already fixed, for every anchor currently modelled

`container.rs:712-750` draws both labels with `Builder::label` (`container.rs:1970-1978`), which:

- uses the real proportional `VanillaFont` (`f.draw_plain`) when a jar font is attached, not the
  fixed-advance 5×7 debug font `ColourStream::text` used before the fix (`container.rs:732-734`'s
  comment names this as the second of three bugs the play report was actually seeing);
- draws with **no drop shadow** — the trailing `false` in vanilla's
  `graphics.text(font, title, titleLabelX, titleLabelY, -12566464, false)`
  (`AbstractContainerScreen.java:189-191`, quoted at `container.rs:714-716`) — matched by
  `label`'s doc comment (`container.rs:1960-1969`) and the fact it never calls the shadowed
  `VanillaFont::draw`;
- uses vanilla's colour `-12566464` = `0xFF404040` = `(64,64,64)/255` when real background art is
  attached (`container.rs:739-743`), falling back to a warm-light ink only on the jar-less
  programmatic-panel path (documented divergence, not a bug — the flat fallback fill is itself
  dark, so vanilla's dark grey would be invisible on it).

### Placement — correct for 3 anchor sets, wrong (defaults to `(8,6)`) for 10 real screens

`label_layout` (`container.rs:338-351`) has exactly two branches:

```rust
pub fn label_layout(menu: &Menu, layout: &SlotLayout) -> LabelLayout {
    match menu.kind() {
        MenuKind::Player => LabelLayout { title_x: 97.0, title_y: 6.0, inventory: None },
        MenuKind::Generic { .. } => LabelLayout {
            title_x: if menu.craft_layout().is_some() { 29.0 } else { 8.0 },
            title_y: 6.0,
            inventory: Some([8.0, layout.height - 94.0]),
        },
    }
}
```

Its own doc comment (`container.rs:331-336`) already flags the gap: *"Not modelled:
`AbstractFurnaceScreen.java:39` centres its title... There is no furnace `MenuKind` yet — a furnace
arrives here as a `Generic` and gets `x = 8`."* That is true today for every one of these, read
from `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/`:

| wire `menu_type` | screen | vanilla `titleLabelX` | vanilla `titleLabelY` | source |
|---|---|---|---|---|
| `minecraft:furnace` | `FurnaceScreen` | `(imageWidth − font.width(title)) / 2` | `6` | `AbstractFurnaceScreen.java:39` |
| `minecraft:blast_furnace` | `BlastFurnaceScreen` | same (inherits `AbstractFurnaceScreen`) | `6` | `AbstractFurnaceScreen.java:39` |
| `minecraft:smoker` | `SmokerScreen` | same | `6` | `AbstractFurnaceScreen.java:39` |
| `minecraft:brewing_stand` | `BrewingStandScreen` | `(imageWidth − font.width(title)) / 2` | `6` | `BrewingStandScreen.java:25` |
| `minecraft:generic_3x3` | `DispenserScreen` (dispenser **and** dropper) | `(imageWidth − font.width(title)) / 2` | `6` | `DispenserScreen.java:20` |
| `minecraft:crafter_3x3` | `CrafterScreen` | `(imageWidth − font.width(title)) / 2` | `6` | `CrafterScreen.java:33` |
| `minecraft:anvil` | `AnvilScreen` | `60` (fixed, not centred) | `6` | `AnvilScreen.java:30` |
| `minecraft:loom` | `LoomScreen` | `8` (default) | `4` (`6 − 2`) | `LoomScreen.java:68` |
| `minecraft:stonecutter` | `StonecutterScreen` | `8` (default) | `5` (`6 − 1`) | `StonecutterScreen.java:45` |
| `minecraft:cartography_table` | `CartographyTableScreen` | `8` (default) | `4` (`6 − 2`) | `CartographyTableScreen.java:29` |

All of the above are currently drawn at `label_layout`'s `(8, 6)` Generic default, which is wrong for
9 of the 10 (loom's `x` happens to already be right; only its `y` is off).

**Every other server-openable menu type already matches `(8, 6)` in vanilla too**, so they need no
change: `minecraft:grindstone` (no override, `GrindstoneScreen.java`), `minecraft:hopper`
(`HopperScreen.java:14` — only `imageHeight` differs, title stays default), `minecraft:shulker_box`
(`ShulkerBoxScreen.java:14` — same), `minecraft:enchantment` (`EnchantmentScreen.java` — no
override), and every `minecraft:generic_9x1..9x6` (`ContainerScreen.java` — no override). `minecraft:crafting`
is already correctly modelled at `x = 29` via the existing `craft_layout().is_some()` branch.

**Deliberately out of scope** (see §5): `minecraft:beacon` (`BeaconScreen.java:46` — 230×219 image,
own `extractLabels` override, no background art of any kind in this crate) and `minecraft:merchant`
(`MerchantScreen.java:85-98` — 276×166 image, composes trade-level text into the title, own
background). Both need their own background/slot-layout work before a title anchor is meaningful;
patching only the title would draw the right text over a still-wrong-shaped panel.

---

## 4. Named containers — full chain confirmed, no island

The server, not the client, decides whether a container's title is its default translatable or a
custom name — our client has no separate "is this custom?" branch to write, because there is no
client-side choice to make. Confirmed at the source:

1. `BaseContainerBlockEntity.getDisplayName()`/`getName()`
   (`.cache/mc/26.2/client-src/net/minecraft/world/level/block/entity/BaseContainerBlockEntity.java:51-53`):
   `return this.name != null ? this.name : this.getDefaultName();` — `this.name` is the `CustomName`
   NBT tag, loaded at `:39` (`parseCustomNameSafe(input, "CustomName")`).
2. `ServerPlayer.openMenu` (`.cache/mc/26.2/src/net/minecraft/server/level/ServerPlayer.java:1352`):
   `this.connection.send(new ClientboundOpenScreenPacket(menu.containerId, menu.getType(), provider.getDisplayName()));`
   — the packet's title is *always* whatever `getDisplayName()` returned, custom or default; the
   wire format cannot distinguish the two cases, and does not need to.

So the entire question is whether our client (a) decodes the title `Component` faithfully whether
it is `literal` or `translate`, and (b) carries it through to the draw call without dropping or
re-deriving it. Traced end to end:

| hop | file:line | does it carry the title through |
|---|---|---|
| decode | `crates/protocol/v770/src/adapter.rs:1383,1388` | `Text::from_nbt(&title)` — preserves literal vs. translate, no early flattening |
| event | `crates/lodestone-model/src/event.rs:1239-1245` | `ClientEvent::ScreenOpened { title: Text, .. }` |
| router | `crates/lodestone-ecs/src/session.rs:492` | `ScreenOpened` **is** listed in `session::handles_event`'s match — confirmed present, not a stale claim (grepped the live file, not a note about it) |
| apply | `crates/lodestone-ecs/src/session.rs:530` (`apply_menus`) → `crates/lodestone-game/src/menus.rs:224-236` | `Menus::apply` stores `title` verbatim on the `pending`/`opened` record |
| read | `crates/lodestone-game/src/menus.rs:199` | `Menus::opened_title()` |
| snapshot | `crates/lodestone-shell/src/sim.rs:2470-2480` | `Sim::open_menu()` copies it into `OpenMenuSnapshot::title` |
| resolve | `crates/lodestone-shell/src/app.rs:2257` | `container::menu_title(&open.title, translator)` — the language table only changes the *rendering* of a `translate` node; a `literal` custom name passes through untouched |
| draw | `crates/lodestone-shell/src/app.rs:2293`, `container.rs:744-750` | `ContainerFrame::new(container_menu, &container_title)` → `b.label(frame.title, ...)` |

No router drops it: `ScreenOpened` is not in `ingest::handles_event`'s per-entity switch (correctly
absent — a screen title is not per-entity state) and is correctly present in
`session::handles_event` (per-entity vs. session-scalar is the fork CLAUDE.md's §1 warns about
guessing; this one is right). `net.rs`'s shell-stream `forward` is not involved for this event at
all — the title travels through `SessionMenus`, not the `ClientEvent` shell stream — so that router
is not a candidate island site here either.

**This is also pixel-gated already**, not just data-flow-traced:
`crates/lodestone-shell/tests/container_labels.rs::a_custom_name_reaches_the_panel_verbatim_and_nothing_is_uppercased`
(line 498) builds a menu with a literal custom title (`"Bob's Loot"`), asserts the resolved string
equals it exactly (line 508), and separately asserts the panel draws it un-uppercased.

**Conclusion: claim 4 needs no patch.** It is correct today, confirmed both by tracing the producer
across every router (not just grepping one file) and by an existing pixel gate.

---

## 5. Patch specification

Additive only: a new field, a new function, one override site, and one call-site wiring change.
Nothing existing is modified in a way that changes its signature or behaviour for current callers —
`label_layout` itself is untouched, so none of `container_labels.rs`'s existing calls to it need to
change.

### 5.1 `crates/lodestone-shell/src/container.rs` — add `menu_type` to `ContainerFrame`

Anchor (current text, `container.rs:139-207`):

```rust
/// The container screen to draw for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ContainerFrame<'a> {
    /// Menu contents to draw. `None` draws nothing.
    pub menu: Option<&'a Menu>,
```
…
```rust
    /// `None` (the default) draws no preview, which is what keeps every existing
    /// caller unchanged. See [`with_drag`](Self::with_drag) for what it draws and
    /// why the counts cannot disagree with the release.
    pub drag: Option<(i32, &'a [usize])>,
}

impl<'a> ContainerFrame<'a> {
    /// A frame for an optional menu, with no cursor position — the carried
    /// stack (if any) will not draw. Chain [`with_cursor`](Self::with_cursor)
    /// to supply one.
    #[must_use]
    pub fn new(menu: Option<&'a Menu>, title: &'a str) -> Self {
        Self {
            menu,
            title,
            inventory_label: DEFAULT_INVENTORY_LABEL,
            cursor: None,
            recipe_book: None,
            drag: None,
        }
    }

    /// A frame that deliberately draws nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            menu: None,
            title: "",
            inventory_label: DEFAULT_INVENTORY_LABEL,
            cursor: None,
            recipe_book: None,
            drag: None,
        }
    }
```

Replace with:

```rust
/// The container screen to draw for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ContainerFrame<'a> {
    /// Menu contents to draw. `None` draws nothing.
    pub menu: Option<&'a Menu>,
```
…
```rust
    /// `None` (the default) draws no preview, which is what keeps every existing
    /// caller unchanged. See [`with_drag`](Self::with_drag) for what it draws and
    /// why the counts cannot disagree with the release.
    pub drag: Option<(i32, &'a [usize])>,
    /// The wire `menu_type` from `OPEN_SCREEN` (`OpenMenuSnapshot::menu_type`),
    /// if this frame is a server-opened container. `None` on the player
    /// inventory screen (no packet involved) and on every caller that predates
    /// this field — both degrade to [`label_layout`]'s existing `(8, 6)`/`(29, 6)`
    /// anchors unchanged. Only consulted by [`menu_type_title_anchor`], for the
    /// handful of real screens whose `titleLabelX`/`Y` vanilla overrides away
    /// from those two anchors; see that function's table.
    pub menu_type: Option<&'a lodestone_model::ResourceKey>,
}

impl<'a> ContainerFrame<'a> {
    /// A frame for an optional menu, with no cursor position — the carried
    /// stack (if any) will not draw. Chain [`with_cursor`](Self::with_cursor)
    /// to supply one.
    #[must_use]
    pub fn new(menu: Option<&'a Menu>, title: &'a str) -> Self {
        Self {
            menu,
            title,
            inventory_label: DEFAULT_INVENTORY_LABEL,
            cursor: None,
            recipe_book: None,
            drag: None,
            menu_type: None,
        }
    }

    /// A frame that deliberately draws nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            menu: None,
            title: "",
            inventory_label: DEFAULT_INVENTORY_LABEL,
            cursor: None,
            recipe_book: None,
            drag: None,
            menu_type: None,
        }
    }

    /// Attach the server's own `menu_type` so [`menu_type_title_anchor`] can
    /// correct the title anchor for the handful of real screens
    /// [`label_layout`] does not model (issue: container title placement).
    /// `None` (the default from [`new`](Self::new)) keeps every existing
    /// caller's anchor unchanged.
    #[must_use]
    pub fn with_menu_type(mut self, menu_type: Option<&'a lodestone_model::ResourceKey>) -> Self {
        self.menu_type = menu_type;
        self
    }
```

*(The `…` above elides the untouched doc comments on `title`/`inventory_label`/`cursor`/
`recipe_book` and the untouched `with_inventory_label`/`with_cursor`/`with_recipe_book`/`with_drag`
methods between the two shown spans — do not delete them, only insert the new field/method.)*

### 5.2 `crates/lodestone-shell/src/container.rs` — new function, placed directly after `label_layout` (after line 351, before the `ContainerBackground` doc comment at line ~353)

```rust
/// Vanilla `titleLabelX`/`titleLabelY` overrides for menu types whose real
/// screen is not [`MenuKind::Player`] or a crafting-table-shaped
/// [`MenuKind::Generic`] — i.e. the real screens [`label_layout`]'s own doc
/// comment already names as "Not modelled."
///
/// Read from `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/`:
///
/// | wire `menu_type` | screen | `titleLabelX` | `titleLabelY` | source |
/// |---|---|---|---|---|
/// | `minecraft:furnace` | `FurnaceScreen` | centred | `6` | `AbstractFurnaceScreen.java:39` |
/// | `minecraft:blast_furnace` | `BlastFurnaceScreen` | centred | `6` | `AbstractFurnaceScreen.java:39` |
/// | `minecraft:smoker` | `SmokerScreen` | centred | `6` | `AbstractFurnaceScreen.java:39` |
/// | `minecraft:brewing_stand` | `BrewingStandScreen` | centred | `6` | `BrewingStandScreen.java:25` |
/// | `minecraft:generic_3x3` | `DispenserScreen` (dispenser & dropper) | centred | `6` | `DispenserScreen.java:20` |
/// | `minecraft:crafter_3x3` | `CrafterScreen` | centred | `6` | `CrafterScreen.java:33` |
/// | `minecraft:anvil` | `AnvilScreen` | `60` | `6` | `AnvilScreen.java:30` |
/// | `minecraft:loom` | `LoomScreen` | `8` | `4` | `LoomScreen.java:68` |
/// | `minecraft:stonecutter` | `StonecutterScreen` | `8` | `5` | `StonecutterScreen.java:45` |
/// | `minecraft:cartography_table` | `CartographyTableScreen` | `8` | `4` | `CartographyTableScreen.java:29` |
///
/// "Centred" is vanilla's `(imageWidth − font.width(title)) / 2`, an **integer**
/// division in Java (`titleLabelX` is declared `int`), which truncates toward
/// zero — matched here with `.floor()` since every real title is narrower than
/// the 176px panel, so the numerator is always non-negative. `layout.width` is
/// used rather than a hardcoded `176.0` because it already *is* vanilla's
/// `imageWidth` for every menu type in this table (`slot_layout` never
/// overrides it away from `176.0` for `Player`/`Generic`/`Crafting`; see
/// `slot_layout`'s own literals).
///
/// Every other server-openable menu type already matches [`label_layout`]'s
/// `(8, 6)` default in vanilla too (`grindstone`, `hopper`, `shulker_box`,
/// `enchantment`, every `generic_9x*`), so they are deliberately absent from
/// this table rather than listed as a no-op case — and `minecraft:crafting`
/// is already handled by `label_layout`'s own `craft_layout().is_some()`
/// branch, so it is absent here too.
///
/// **Not covered on purpose**: `minecraft:beacon` and `minecraft:merchant`
/// draw at a different `imageWidth` (230 and 276) with their own background
/// art, and the merchant's `extractLabels` is a full override that composes
/// trade-level text into the title (`MerchantScreen.java:85-98`) rather than
/// just moving `titleLabelX`. Neither has a background or slot layout in this
/// crate ([`background_kind`]/[`slot_layout`] have no case for them), so a
/// title anchor alone would draw correct text over a still-wrong-shaped panel.
/// Fix those together with their own background/layout work, not here.
///
/// Returns `None` for a `menu_type` this table does not know (or `None`
/// itself), so the caller falls back to [`label_layout`]'s own anchor
/// unchanged — this function only ever *overrides*, never replaces, the
/// existing default.
#[must_use]
pub fn menu_type_title_anchor(
    menu_type: Option<&lodestone_model::ResourceKey>,
    layout: &SlotLayout,
    title: &str,
    font: Option<&VanillaFont>,
) -> Option<[f32; 2]> {
    let key = menu_type?;
    if key.namespace() != "minecraft" {
        return None;
    }
    let centred = matches!(
        key.path(),
        "furnace" | "blast_furnace" | "smoker" | "brewing_stand" | "generic_3x3" | "crafter_3x3"
    );
    if centred {
        let text_width = font.map_or(0.0, |f| f.width(title, 1.0));
        return Some([((layout.width - text_width) / 2.0).floor(), 6.0]);
    }
    match key.path() {
        "anvil" => Some([60.0, 6.0]),
        "loom" => Some([8.0, 4.0]),
        "stonecutter" => Some([8.0, 5.0]),
        "cartography_table" => Some([8.0, 4.0]),
        _ => None,
    }
}
```

### 5.3 `crates/lodestone-shell/src/container.rs` — apply the override in `build_inner`

Anchor (current text, `container.rs:736-750`):

```rust
    // `label_layout` supplies the anchors; `titleLabelY` is 6, not 7, and
    // `titleLabelX` is not always 8.
    let labels = label_layout(menu, &layout);
    let label_colour = if bg_quads.is_some() {
        [64.0 / 255.0, 64.0 / 255.0, 64.0 / 255.0, 1.0]
    } else {
        [0.88, 0.84, 0.73, 1.0]
    };
    b.label(
        frame.title,
        x + labels.title_x,
        y + labels.title_y,
        1.0,
        label_colour,
    );
```

Replace with:

```rust
    // `label_layout` supplies the anchors; `titleLabelY` is 6, not 7, and
    // `titleLabelX` is not always 8. `menu_type_title_anchor` then corrects
    // the handful of real screens `label_layout` does not model (furnace,
    // brewing stand, anvil, loom, stonecutter, cartography table,
    // dispenser/dropper, crafter) — see that function's own table. `None`
    // (no `menu_type` attached, or a menu type it does not know) leaves
    // `label_layout`'s anchor untouched.
    let labels = label_layout(menu, &layout);
    let labels = match menu_type_title_anchor(frame.menu_type, &layout, frame.title, font) {
        Some([title_x, title_y]) => LabelLayout {
            title_x,
            title_y,
            ..labels
        },
        None => labels,
    };
    let label_colour = if bg_quads.is_some() {
        [64.0 / 255.0, 64.0 / 255.0, 64.0 / 255.0, 1.0]
    } else {
        [0.88, 0.84, 0.73, 1.0]
    };
    b.label(
        frame.title,
        x + labels.title_x,
        y + labels.title_y,
        1.0,
        label_colour,
    );
```

### 5.4 `crates/lodestone-shell/src/app.rs` — wire the live `menu_type` in

Anchor (current text, `app.rs:2293-2297`):

```rust
            let container_frame = ContainerFrame::new(container_menu, &container_title)
                .with_inventory_label(&inventory_label)
                .with_cursor(Some([self.cursor.0, self.cursor.1]))
                .with_drag(self.menu_input.drag_paint())
                .with_recipe_book(self.recipe_book.as_ref());
```

Replace with:

```rust
            let container_frame = ContainerFrame::new(container_menu, &container_title)
                .with_inventory_label(&inventory_label)
                .with_cursor(Some([self.cursor.0, self.cursor.1]))
                .with_drag(self.menu_input.drag_paint())
                .with_recipe_book(self.recipe_book.as_ref())
                // Only a server-opened menu carries a wire `menu_type`; the
                // player inventory branch above builds `container_title`
                // without an `open` snapshot, so this is `None` there and
                // `label_layout`'s existing `x = 97` anchor is untouched —
                // see `container::menu_type_title_anchor`.
                .with_menu_type(open_menu.as_ref().map(|open| &open.menu_type));
```

This is safe against the player-inventory branch (`self.ui.is_container_open()`, `app.rs:2259-2272`):
`open_menu` is the same `Option<OpenMenuSnapshot>` read once at `app.rs:2248`, so when the player
screen is open (no server menu) this is `None`, exactly matching the current, unchanged behaviour.

### 5.5 New tests — append to `crates/lodestone-shell/tests/container_labels.rs`

Additive only (do not touch any existing test in the file). Two tests: one exact-constant screen
(negative-control-friendly, no font dependency) and one centred screen (exercises the font-width
arithmetic against the file's own font oracle, matching how every other font-metric assertion in
this file already works — see `label_ink`/`assert_at_anchor`, lines 206–290).

```rust
/// `AnvilScreen.java:30`'s fixed `titleLabelX = 60` — not centred, unlike the
/// furnace/dispenser family. A pure constant, so the expected value needs no
/// font: it is asserted directly against the decompile, not derived from
/// anything this crate computes.
#[test]
fn an_anvil_titles_at_the_vanilla_fixed_anchor() {
    let menu = Menu::generic(3); // AnvilMenu: 2 input + 1 result
    let layout = slot_layout(&menu);
    let anchor = lodestone::container::menu_type_title_anchor(
        Some(&"minecraft:anvil".parse().unwrap()),
        &layout,
        "Repair & Name",
        None,
    );
    assert_eq!(
        anchor,
        Some([60.0, 6.0]),
        "AnvilScreen.java:30 fixes titleLabelX at 60, not vanilla's usual 8"
    );
}

/// `AbstractFurnaceScreen.java:39`'s centred title. The expected value is
/// computed from the same `VanillaFont::width` this crate already trusts for
/// every other centred-text assertion in this file (see `label_ink`'s own
/// note on why that is not circular: the font metrics are validated
/// elsewhere, so reusing them here tests the *anchor arithmetic*, not the
/// glyph widths) — Java's `(imageWidth - font.width(title)) / 2` is integer
/// division, truncating toward zero, matched by `.floor()`.
#[test]
fn a_furnace_titles_centred_like_vanillas_abstract_furnace_screen() {
    let Some(font) = VanillaFont::shared() else {
        return; // jar-less run: no font to centre against, nothing to assert
    };
    let menu = Menu::generic(3); // FurnaceMenu: input + fuel + result
    let layout = slot_layout(&menu);
    let title = "Furnace";
    let anchor = lodestone::container::menu_type_title_anchor(
        Some(&"minecraft:furnace".parse().unwrap()),
        &layout,
        title,
        Some(&font),
    );
    let expected_x = ((layout.width - font.width(title, 1.0)) / 2.0).floor();
    assert_eq!(anchor, Some([expected_x, 6.0]));
    // Control: an unrecognised menu type must fall through to `None`, proving
    // this function overrides rather than silently claiming every screen.
    assert_eq!(
        lodestone::container::menu_type_title_anchor(
            Some(&"minecraft:hopper".parse().unwrap()),
            &layout,
            title,
            Some(&font),
        ),
        None,
        "control: hopper already matches label_layout's (8,6) default and must not be overridden"
    );
}
```

The file's existing `use` block (`container_labels.rs:60-64`) already imports `label_layout`,
`slot_layout`, `ContainerFrame`, `ContainerGeometry`, `ContainerRenderer`, `LabelLayout`,
`panel_origin` from `lodestone::container`, and `Menu` from `lodestone_game::menu` — add
`menu_type_title_anchor` to that same `lodestone::container::{...}` list. `VanillaFont` is **not**
currently imported in this file (other pixel gates in this crate reach it via
`lodestone::hud::VanillaFont` — e.g. `tests/vanilla_font_pixels.rs`,
`tests/container_background_pixels.rs` — a valid public path since `lodestone-shell/src/lib.rs:39`
has `pub mod hud;`); add `use lodestone::hud::VanillaFont;` alongside the existing imports.

---

## How to prove it

**Gate**: the two new tests above, run narrowly —
`cargo test -p lodestone-shell --test container_labels -- --nocapture` (this is a `--test <binary>`
run, which CLAUDE.md flags as a narrower filter than even `-p` fail-fast; it is acceptable here only
because these are brand-new tests with no risk of masking an unrelated red binary — for a real
merge, run the crate with `--no-fail-fast` as the rules require).

**Expected value, and where it comes from outside this code**:
- Anvil: the literal `60.0` from `AnvilScreen.java:30`, typed by hand from the decompile — not
  derived from anything in this crate.
- Furnace: `(176.0 − font.width("Furnace", 1.0)) / 2.0`, floored. The `176.0` (`imageWidth`) comes
  from `AbstractContainerScreen`'s `DEFAULT_IMAGE_WIDTH` constant (`AbstractContainerScreen.java:33`),
  not from `layout.width` being asserted equal to itself — that equality is `slot_layout`'s own
  existing job, already covered by `slot_layout_height_is_vanillas_image_height`
  (`container_labels.rs:302`). The font-width term reuses this crate's own `VanillaFont`, which is
  legitimate here (see the test's doc comment) because the thing under test is the **centring
  arithmetic**, not the glyph metrics — the same posture this file already takes for every other
  centred-text assertion.

**Negative control**: `menu_type_title_anchor(Some(hopper), …)` must return `None` — proving the
function does not simply override every screen's anchor regardless of type, only the ones vanilla
actually moves. This is the control CLAUDE.md's evidence-standards section asks for ("assertions of
an absence need a control proving the detector works").

**What else already paints in this rect, checked before trusting the control**: for `Menu::generic(3)`
(used for both the anvil and furnace tests), `background_kind` (`container.rs:399-408`) resolves to
`BackgroundKind::Generic { rows: 1 }` — the plain chest-style fallback, not a furnace/anvil texture
(neither exists in `ContainerBackground`; see §3's "deliberately out of scope" note extended to
these too, though title-anchor correctness does not depend on the background being right). Nothing
else in this widget rect draws with the label colour except the two labels themselves —
`assert_nothing_else_uses_the_label_colour` (`container_labels.rs:173`) already establishes this as
a general premise for the file, and the new tests do not need to re-derive it since they assert on
`menu_type_title_anchor`'s **return value** directly, not on rendered ink — no bounding-box gate is
needed for this part because there is no rendering step in these two tests; a follow-up pixel gate
(mirroring `a_chest_screen_draws_both_labels_at_their_derived_anchors`, `container_labels.rs:369`)
would be the natural next step once someone wants a true end-to-end pixel proof, but is not required
to prove the arithmetic itself is right.

---

## What was ruled out

- **Not an island.** Traced `ScreenOpened` through both `ingest::handles_event` and
  `session::handles_event` by reading the live files (not grepping a note about them): it is
  correctly absent from the per-entity router and correctly present in the session-scalar router
  (`crates/lodestone-ecs/src/session.rs:492`). `net.rs`'s shell-stream `forward` is not on this
  event's path at all — `SessionMenus` is written directly by `apply_menus`
  (`session.rs:530`), not via the `ClientEvent` shell stream — so that third router was never a
  candidate here.
- **Not a stale-comment trap.** The doc comments in `container.rs` and `text.rs` that describe the
  #52/#370 fixes were cross-checked against the *current* function bodies, not taken on faith — the
  uppercasing call, the debug-font call, and the raw-key resolution they each describe as fixed are
  in fact absent from the code that runs today.
- **Not a font/colour/shadow bug.** All three are correct for every anchor `label_layout` currently
  computes; confirmed by reading `Builder::label` and its call site directly, not by trusting the
  comments describing them.
- **Not the background/slot-layout gap** for furnace, anvil, brewing stand, etc. — that gap is real
  (`background_kind` has no case for any of them; they all render as a chest-shaped box today) but
  is **out of scope for this patch**, which only touches the title anchor. Fixing the background
  art and slot geometry for these container types is a separate, materially larger piece of work
  (new texture sheets, non-grid slot positions — a furnace's 3 slots are not `n` cells in a row).
  Noting this rather than silently leaving it for someone to rediscover.
- **Beacon and merchant title anchors** were investigated (`BeaconScreen.java:46`,
  `MerchantScreen.java:85-98`) and deliberately excluded from the patch for the same
  background/layout reason, plus the merchant's title is not a simple anchor move — it composes
  trade-level text in with the base title.
- **Did not run any GPU/pixel gate** for this investigation — read-only, and the existing
  `container_labels.rs` gates already establish the harness works; a live oracle screenshot was not
  taken because the string/font/placement evidence all traces to decompiled source and the current
  code, which is a stronger source than a screenshot per CLAUDE.md's evidence standards ("expected
  value must originate outside the code under test" — the Java source is exactly that).
