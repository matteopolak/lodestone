//! Gate: **a container screen's two labels say the right words, in the right
//! typeface, at vanilla's own anchors — and the one screen that omits the second
//! label omits it there and nowhere else** (issue #370).
//!
//! # What is measured, and why by location
//!
//! Every assertion here is a **bounding box in local widget pixels**, never a
//! coverage fraction. `CLAUDE.md`'s standing complaint applies exactly: a
//! fraction cannot tell a label drawn 20 px off from a label not drawn at all,
//! and that confusion has cost this repo two sessions. Failure output prints the
//! measured box and the derived anchor next to each other.
//!
//! # How the label ink is isolated
//!
//! Not by assuming nothing else paints in the label's rect — a control's premise
//! being false *before* the feature existed is a documented failure mode here
//! (the first-person bare arm; the HUD's moving `cluster_top`). Instead the ink
//! is isolated by **subtraction**: geometry is built twice, once with the label
//! under test blanked to `""` and once with it set, and the run of vertices
//! present only in the second is the label. [`assert_nothing_else_uses_the_label_colour`]
//! then closes the loop from the other side by proving the blanked build emits no
//! vertex of the label colour anywhere, so the subtraction cannot have picked up
//! something else's ink.
//!
//! # The anchors are derived, not restated
//!
//! `inventoryLabelY = imageHeight - 94` and `imageHeight` moves with a chest's
//! row count, so the expected value comes from
//! [`label_layout`](lodestone::container::label_layout) over the same
//! [`SlotLayout`](lodestone::container::SlotLayout) the panel is drawn from —
//! which is also what the gate re-derives independently below, from vanilla's
//! `114 + rows * 18`.
//!
//! # The controls were run, and these are what they printed
//!
//! Not "would fail" — each was armed, executed, and observed failing:
//!
//! * **The absence detector fires.** Pointing
//!   [`the_player_inventory_screen_omits_the_second_label_and_titles_at_97`]'s
//!   absence assertion at a *chest* screen fails with
//!   `Some(Bbox { x0: 9.0, y0: 74.0, x1: 61.0, y1: 81.0 })`. "Returned `None`" is
//!   therefore a measurement and not a detector that never fires.
//! * **The pre-#370 draw fails five of these seven tests.** Restoring the old
//!   `b.text(&title.to_ascii_uppercase(), x + 8.0, y + 7.0, ..)` and deleting the
//!   second label reproduces exactly the reported screen, and the boxes say so:
//!   every title at `x 8.0..`, `y 7.0..14.0` regardless of screen (`Crafting`
//!   measured at `x 8.0..55.0` against an anchor of `97`), no second label at any
//!   row count, and `"Bob's Loot"` and `"BOB'S LOOT"` emitting an identical 768
//!   ink vertices — a renamed chest shouting.
//! * **The no-shadow assertion fires.** Giving `VanillaFont::draw_plain` a shadow
//!   pass again takes the darkest label pixel from `[64, 64, 64]` to
//!   `[16, 16, 16]` — 25 % of the ink, exactly as predicted — and the GPU gate
//!   fails on it.
//!
//! ```text
//! cargo test -p lodestone-shell --test container_labels
//! cargo test -p lodestone-shell --test container_labels -- --ignored --nocapture
//! ```

use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::container::{
    ContainerFrame, ContainerGeometry, ContainerRenderer, LabelLayout, label_layout, panel_origin,
    slot_layout,
};
use lodestone_game::menu::Menu;

/// Chosen so `calculate_gui_scale(AUTO, W, H) == 1` — the physical/logical
/// canvas divide is a no-op, matching every sibling gate in this crate.
const W: u32 = 480;
const H: u32 = 320;

const FLOATS_PER_VERTEX: usize = 6;

/// The label ink colour on the **jar-less** path — `container.rs`'s
/// `label_colour` when no real panel art is attached. Deliberately not vanilla's
/// `0xFF404040`: the programmatic fallback panel is dark, and dark grey on it
/// would be invisible. The GPU gate at the bottom of this file is what asserts
/// the vanilla value, on the path that has the art.
const FALLBACK_LABEL_INK: [f32; 4] = [0.88, 0.84, 0.73, 1.0];

/// Vanilla's `-12566464`, i.e. `0xFF404040`, as bytes.
const VANILLA_LABEL_RGB: [u8; 3] = [0x40, 0x40, 0x40];

/// A measured box in **local widget pixels** (panel origin subtracted), so it
/// compares directly against `label_layout`'s anchors.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bbox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl std::fmt::Display for Bbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "x {:.1}..{:.1}, y {:.1}..{:.1} ({:.1}x{:.1})",
            self.x0,
            self.x1,
            self.y0,
            self.y1,
            self.x1 - self.x0,
            self.y1 - self.y0
        )
    }
}

/// Which of vanilla's two labels a measurement is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    /// `AbstractContainerScreen.title`.
    Title,
    /// `AbstractContainerScreen.playerInventoryTitle`.
    PlayerInventory,
}

fn scale() -> f32 {
    calculate_gui_scale(AUTO_GUI_SCALE, W, H).max(1) as f32
}

/// The logical canvas `ContainerGeometry` poses into — the same division
/// `menu::render::logical_canvas` performs, restated here because the test is
/// outside the crate.
fn logical_canvas() -> (f32, f32) {
    let s = scale();
    (W as f32 / s, H as f32 / s)
}

fn frame<'a>(menu: &'a Menu, title: &'a str, inventory_label: &'a str) -> ContainerFrame<'a> {
    ContainerFrame::new(Some(menu), title).with_inventory_label(inventory_label)
}

/// Every vertex of `verts` whose RGBA is `c`, as `(x_px, y_px)` in local widget
/// pixels.
fn ink_of_colour(verts: &[f32], c: [f32; 4], origin: (f32, f32)) -> Vec<(f32, f32)> {
    let (cw, ch) = logical_canvas();
    verts
        .chunks_exact(FLOATS_PER_VERTEX)
        .filter(|v| v[2..6] == c)
        .map(|v| {
            (
                (v[0] + 1.0) * cw * 0.5 - origin.0,
                (1.0 - v[1]) * ch * 0.5 - origin.1,
            )
        })
        .collect()
}

fn bbox_of(points: &[(f32, f32)]) -> Option<Bbox> {
    let mut it = points.iter();
    let &(x, y) = it.next()?;
    let mut b = Bbox {
        x0: x,
        y0: y,
        x1: x,
        y1: y,
    };
    for &(x, y) in it {
        b.x0 = b.x0.min(x);
        b.y0 = b.y0.min(y);
        b.x1 = b.x1.max(x);
        b.y1 = b.y1.max(y);
    }
    Some(b)
}

/// The premise every subtraction below rests on, asserted rather than assumed:
/// with **both** labels blanked, no vertex anywhere in the screen carries the
/// label ink colour. If something else did paint in it, the extracted "label ink"
/// could be that something else, and the gate would be measuring an unrelated
/// thing while looking rigorous.
fn assert_nothing_else_uses_the_label_colour(menu: &Menu) {
    let blank = ContainerGeometry::build(&frame(menu, "", ""), W, H);
    let origin = panel_origin(&slot_layout(menu), W, H);
    let stray = ink_of_colour(&blank.verts, FALLBACK_LABEL_INK, origin);
    assert!(
        stray.is_empty(),
        "premise check: with both labels blanked, nothing in this screen may draw \
         in the label ink colour {FALLBACK_LABEL_INK:?} — found {} such vertices, \
         bbox {:?}. Every measurement in this file subtracts one build from \
         another and would silently attribute that ink to a label.",
        stray.len(),
        bbox_of(&stray)
    );
}

/// One label's isolated ink.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ink {
    /// Where it landed, in local widget pixels.
    bbox: Bbox,
    /// How many vertices of ink it emitted. The bounding box alone cannot see a
    /// *casing* change under the fixed-advance fallback font — "Bob's Loot" and
    /// "BOB'S LOOT" occupy the identical 59×7 box there — but the glyph bitmaps
    /// differ, so the quad count does.
    verts: usize,
}

/// Isolate one label's ink by subtraction, or `None` when the label emitted no
/// geometry at all.
///
/// `title`/`inventory_label` are the words the subject build uses; the baseline
/// build blanks whichever one `which` names and leaves the other alone, so the
/// two labels are separable even though they share a colour.
fn label_ink(menu: &Menu, title: &str, inventory_label: &str, which: Which) -> Option<Ink> {
    let (base_title, base_label) = match which {
        Which::Title => ("", inventory_label),
        Which::PlayerInventory => (title, ""),
    };
    let base = ContainerGeometry::build(&frame(menu, base_title, base_label), W, H);
    let subject = ContainerGeometry::build(&frame(menu, title, inventory_label), W, H);

    let extra = subject.verts.len().checked_sub(base.verts.len())?;
    if extra == 0 {
        return None;
    }
    // The label's vertices are inserted contiguously: everything the builder
    // emits after them is positioned independently of the label's text, so the
    // subject stream is `prefix ++ ink ++ suffix` and the baseline is
    // `prefix ++ suffix`.
    //
    // Rounded **down to a whole vertex**, and that is load-bearing. The first
    // *float* the two streams disagree on is not necessarily the start of a
    // vertex: a chest's title and its second label both sit at `x = 8`, so the
    // first float of each — the x coordinate — is bit-identical and the scan
    // skips it. Reading from a one-float offset reinterprets `(y, r)` as
    // `(x, y)`, which is how this first ran: a title measured at
    // `x 194.5..205.0, y -56.8..-56.8`, outside the panel and with zero height.
    // A misaligned window still satisfies the suffix check below, because the
    // shift is consistent on both sides of it.
    let prefix = base
        .verts
        .iter()
        .zip(&subject.verts)
        .position(|(a, b)| a != b)
        .unwrap_or(base.verts.len())
        / FLOATS_PER_VERTEX
        * FLOATS_PER_VERTEX;
    assert_eq!(
        base.verts[prefix..],
        subject.verts[prefix + extra..],
        "the two builds must differ only by one contiguous run of label vertices; \
         if this fires the subtraction has mis-aligned and every box below is \
         measuring the wrong vertices"
    );
    let origin = panel_origin(&slot_layout(menu), W, H);
    let ink = &subject.verts[prefix..prefix + extra];
    // Positions only — the run is by construction all label ink, so filtering by
    // colour here as well would only mask a colour regression.
    let (cw, ch) = logical_canvas();
    let points: Vec<(f32, f32)> = ink
        .chunks_exact(FLOATS_PER_VERTEX)
        .map(|v| {
            (
                (v[0] + 1.0) * cw * 0.5 - origin.0,
                (1.0 - v[1]) * ch * 0.5 - origin.1,
            )
        })
        .collect();
    Some(Ink {
        bbox: bbox_of(&points)?,
        verts: points.len(),
    })
}

/// [`label_ink`]'s box alone, for the placement assertions.
fn label_bbox(menu: &Menu, title: &str, inventory_label: &str, which: Which) -> Option<Bbox> {
    label_ink(menu, title, inventory_label, which).map(|i| i.bbox)
}

/// A label's ink must start at its anchor (within a glyph's own left/top bearing)
/// and stay on one 8 px line. `2.0` is the slack a bearing can add; a label at
/// the *wrong* anchor misses by tens of pixels, which is the discrimination this
/// gate exists for.
fn assert_at_anchor(what: &str, got: Bbox, anchor: [f32; 2]) {
    let [ax, ay] = anchor;
    eprintln!("  {what:<22} bbox {got}   anchor ({ax:.1}, {ay:.1})");
    assert!(
        got.x0 >= ax - 0.5 && got.x0 <= ax + 2.0,
        "{what}: ink must start at the derived anchor x={ax:.1}; measured box {got}"
    );
    assert!(
        got.y0 >= ay - 0.5 && got.y0 <= ay + 2.0,
        "{what}: ink must start at the derived anchor y={ay:.1}; measured box {got}"
    );
    assert!(
        got.y1 <= ay + 9.0,
        "{what}: a vanilla label is one 8 px line at y={ay:.1}; measured box {got} \
         runs past it — a second line, a drop shadow, or the wrong anchor"
    );
}

/// `SlotLayout::height` must *be* vanilla's `imageHeight`, because
/// `inventoryLabelY` is derived from it. Checked against vanilla's own
/// constructors rather than against our own layout function, so the two cannot
/// agree by sharing a mistake:
///
/// * `ContainerScreen.java:16` — `super(..., 176, 114 + rowCount * 18)`
/// * `AbstractContainerScreen`'s recipe-book subclasses — `176 x 166`
#[test]
fn slot_layout_height_is_vanillas_image_height() {
    for rows in 1..=6u32 {
        let menu = Menu::generic(rows as usize * 9);
        let got = slot_layout(&menu).height;
        let want = 114.0 + rows as f32 * 18.0;
        assert_eq!(
            got, want,
            "a {rows}-row chest's imageHeight is 114 + rows * 18 = {want} \
             (ContainerScreen.java:16); slot_layout says {got}. inventoryLabelY is \
             derived from this, so a wrong height moves the label."
        );
    }
    assert_eq!(slot_layout(&Menu::player()).height, 166.0);
    assert_eq!(slot_layout(&Menu::crafting(3, 3)).height, 166.0);
}

/// The anchors themselves, before any pixel is measured.
#[test]
fn label_anchors_match_vanillas_four_fields() {
    let player = Menu::player();
    let chest = Menu::generic(27);
    let big_chest = Menu::generic(54);
    let table = Menu::crafting(3, 3);

    // `InventoryScreen.java:29` — pushed right, past the player model panel —
    // and `:73-75`, the `extractLabels` override that drops the second call.
    assert_eq!(
        label_layout(&player, &slot_layout(&player)),
        LabelLayout {
            title_x: 97.0,
            title_y: 6.0,
            inventory: None,
        }
    );
    // `CraftingScreen.java:22`. Note this one *does* keep the second label:
    // `InventoryScreen` is the only `extractLabels` override in the package.
    assert_eq!(
        label_layout(&table, &slot_layout(&table)),
        LabelLayout {
            title_x: 29.0,
            title_y: 6.0,
            inventory: Some([8.0, 166.0 - 94.0]),
        }
    );
    // `AbstractContainerScreen.java:68-71`, with `imageHeight` moving under it.
    assert_eq!(
        label_layout(&chest, &slot_layout(&chest)),
        LabelLayout {
            title_x: 8.0,
            title_y: 6.0,
            inventory: Some([8.0, 114.0 + 3.0 * 18.0 - 94.0]),
        }
    );
    let six = label_layout(&big_chest, &slot_layout(&big_chest));
    assert_eq!(six.inventory, Some([8.0, 114.0 + 6.0 * 18.0 - 94.0]));
    assert_ne!(
        six.inventory,
        label_layout(&chest, &slot_layout(&chest)).inventory,
        "a 6-row chest's second label sits 54 px lower than a 3-row chest's; if \
         these are equal the anchor has been restated as a constant instead of \
         derived from imageHeight"
    );
}

/// The four claims, measured on the generic container screen: both labels draw,
/// each at its own derived anchor.
#[test]
fn a_chest_screen_draws_both_labels_at_their_derived_anchors() {
    let menu = Menu::generic(27);
    assert_nothing_else_uses_the_label_colour(&menu);
    let anchors = label_layout(&menu, &slot_layout(&menu));
    let inventory_anchor = anchors
        .inventory
        .expect("a generic container draws the second label");

    eprintln!("=== chest screen (27 slots) ===");
    let title = label_bbox(&menu, "Chest", "Inventory", Which::Title)
        .expect("the title must draw on a chest screen");
    assert_at_anchor("title 'Chest'", title, [anchors.title_x, anchors.title_y]);

    let label = label_bbox(&menu, "Chest", "Inventory", Which::PlayerInventory).expect(
        "a chest screen draws `playerInventoryTitle` — `AbstractContainerScreen.java:191`, \
         which only `InventoryScreen` overrides away",
    );
    assert_at_anchor("second 'Inventory'", label, inventory_anchor);

    assert!(
        label.y0 > title.y1,
        "the second label sits over the player's own storage rows, far below the \
         title: title {title}, second label {label}"
    );
}

/// The row count moves the second label, and the gate must see it move. A
/// hardcoded `inventoryLabelY` passes the 3-row case and fails here.
#[test]
fn the_second_label_follows_image_height_across_row_counts() {
    eprintln!("=== second label vs row count ===");
    let mut seen = Vec::new();
    for rows in [1usize, 3, 6] {
        let menu = Menu::generic(rows * 9);
        let anchor = label_layout(&menu, &slot_layout(&menu))
            .inventory
            .expect("a generic container draws the second label");
        let got = label_bbox(&menu, "Chest", "Inventory", Which::PlayerInventory)
            .expect("the second label must draw at every row count");
        eprintln!("  rows={rows}");
        assert_at_anchor("second 'Inventory'", got, anchor);
        seen.push(got.y0);
    }
    assert!(
        seen[0] < seen[1] && seen[1] < seen[2],
        "the second label must move down as the chest grows; got y0 {seen:?}"
    );
}

/// The crafting table's own title anchor — `x = 29`, not the base class's `8`,
/// and not the inventory screen's `97`.
#[test]
fn a_crafting_table_draws_its_title_at_29_and_keeps_the_second_label() {
    let menu = Menu::crafting(3, 3);
    assert_nothing_else_uses_the_label_colour(&menu);
    let anchors = label_layout(&menu, &slot_layout(&menu));

    eprintln!("=== crafting table ===");
    let title = label_bbox(&menu, "Crafting Table", "Inventory", Which::Title)
        .expect("the title must draw on a crafting table screen");
    assert_at_anchor("title", title, [anchors.title_x, anchors.title_y]);
    assert!(
        title.x0 > 20.0 && title.x0 < 40.0,
        "CraftingScreen.java:22 sets titleLabelX = 29; a box at x0={:.1} is either \
         the base class's 8 or the inventory screen's 97",
        title.x0
    );

    let anchor = anchors.inventory.expect("a crafting table keeps both labels");
    let label = label_bbox(&menu, "Crafting Table", "Inventory", Which::PlayerInventory)
        .expect("a crafting table does not override extractLabels");
    assert_at_anchor("second 'Inventory'", label, anchor);
}

/// The player inventory screen: title `"Crafting"` at `x = 97`, and **no second
/// label at all**.
///
/// The absence claim carries its own executed control — the same
/// [`label_bbox`] call, `Which::PlayerInventory`, against a chest screen, where
/// the label is correct and present. Without it "returned `None`" is worth
/// nothing: a detector that always returns `None` satisfies it.
#[test]
fn the_player_inventory_screen_omits_the_second_label_and_titles_at_97() {
    let player = Menu::player();
    assert_nothing_else_uses_the_label_colour(&player);
    let anchors = label_layout(&player, &slot_layout(&player));

    eprintln!("=== player inventory screen ===");
    let title = label_bbox(&player, "Crafting", "Inventory", Which::Title)
        .expect("the inventory screen still draws its title");
    assert_at_anchor("title 'Crafting'", title, [anchors.title_x, anchors.title_y]);
    assert!(
        title.x0 > 90.0,
        "InventoryScreen.java:29 sets titleLabelX = 97, past the player model \
         panel; a title box at x0={:.1} is the base class's 8",
        title.x0
    );

    // The claim.
    let absent = label_bbox(&player, "Crafting", "Inventory", Which::PlayerInventory);
    // The control: the identical call, on the screen where the label belongs.
    let chest = Menu::generic(27);
    let present = label_bbox(&chest, "Chest", "Inventory", Which::PlayerInventory);
    eprintln!("  player inventory second label = {absent:?}");
    eprintln!("  chest screen second label     = {present:?}  (control)");
    assert!(
        present.is_some(),
        "executed control: this same detector must find the second label on a chest \
         screen. It returned None, so the absence assertion below proves nothing — \
         a detector that never fires satisfies it."
    );
    assert!(
        absent.is_none(),
        "InventoryScreen.extractLabels draws only the title \
         (`InventoryScreen.java:73-75`); the player inventory screen must emit no \
         `playerInventoryTitle` geometry, but one was measured at {absent:?}"
    );
}

/// Text claims, separate from placement: no uppercasing, and a custom name
/// arriving as a `Text` on the packet path reaches the screen verbatim.
///
/// The *world* species of vacuous test is the risk here — a title string set
/// directly on the screen would exercise nothing, because the string is not what
/// was ever in doubt. So the custom name is built as
/// `Text::literal("Bob's Loot")`, the shape `OPEN_SCREEN` actually delivers for a
/// chest renamed in an anvil, and run through `menu_title` with a language table
/// that would happily mangle a key.
#[test]
fn a_custom_name_reaches_the_panel_verbatim_and_nothing_is_uppercased() {
    let lang = |key: &str| match key {
        "container.chest" => Some("Chest".to_owned()),
        "container.crafting" => Some("Crafting".to_owned()),
        "container.inventory" => Some("Inventory".to_owned()),
        _ => None,
    };
    // What the server sends for a renamed chest: a literal, not a key.
    let renamed = lodestone_model::Text::literal("Bob's Loot");
    let resolved = lodestone::container::menu_title(&renamed, &lang);
    assert_eq!(resolved, "Bob's Loot");
    assert_eq!(
        lodestone::container::player_inventory_title(&lang),
        "Crafting",
        "the player inventory screen's title is container.crafting — the 2x2 grid \
         (InventoryScreen.java:28), never the word Inventory"
    );
    assert_eq!(lodestone::container::player_inventory_label(&lang), "Inventory");

    // Uppercasing is the observable that used to make a custom name unreadable:
    // "Bob's Loot" drew as "BOB'S LOOT".
    //
    // The bounding box **cannot** see this, and finding that out is worth more
    // than the assertion: under the jar-less fixed-advance font both strings
    // occupy the identical 59x7 box, because every glyph is 5 px wide and no
    // glyph in that font has a descender. Measured, not reasoned — the first
    // version of this test compared boxes and passed a folded-case draw. The
    // glyph *bitmaps* differ, so the ink vertex count does.
    let menu = Menu::generic(27);
    let mixed =
        label_ink(&menu, &resolved, "Inventory", Which::Title).expect("a renamed chest's title must draw");
    let shouted = label_ink(&menu, &resolved.to_uppercase(), "Inventory", Which::Title)
        .expect("the control must draw too");
    eprintln!("=== casing ===");
    eprintln!("  \"Bob's Loot\" {} in {} ink verts", mixed.bbox, mixed.verts);
    eprintln!(
        "  \"BOB'S LOOT\" {} in {} ink verts   (what the old code drew)",
        shouted.bbox, shouted.verts
    );
    assert_eq!(
        mixed.bbox, shouted.bbox,
        "a note for whoever tightens this next: under the fallback font casing is \
         invisible to the box, which is why the count below is the assertion. If \
         this ever stops holding, the fallback font grew proportional advances or \
         descenders and the box became usable."
    );
    assert_ne!(
        mixed.verts, shouted.verts,
        "the title must be drawn as given; if mixed case and upper case emit the \
         same ink the panel is still folding the case and a renamed container \
         still shouts"
    );
}

/// Pixel gate for the two claims that only exist once the real assets are
/// present: vanilla's `-12566464` ink, and **no drop shadow**.
///
/// The shadow is what makes this a pixel question rather than a geometry one.
/// `VanillaFont::draw` emits a second copy of every glyph at `+1, +1` in
/// `shadow_of(colour)` — 25 % of each channel, so `0x404040` would cast a
/// `0x101010` outline. The gate reads back the title band and asserts the darkest
/// pixel in it is the ink colour and nothing darker, with the un-dimmed panel art
/// as the reference for "what else already paints here".
///
/// ```text
/// cargo test -p lodestone-shell --test container_labels -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_labels_draw_in_vanillas_dark_grey_with_no_drop_shadow() {
    use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "the widget-pixel maths below assumes the logical canvas is the framebuffer"
    );
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let background = lodestone::resources::load_container_background().expect(
        "GPU gate opted in but the vanilla container background did not load; set \
         LODESTONE_ASSETS to a pack root with client.jar",
    );
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut renderer = ContainerRenderer::new(device, format);
    renderer.attach_background(device, queue, format, background);
    assert!(
        renderer.background_attached(),
        "without the real panel art the ink colour under test is the fallback's \
         warm light, not vanilla's -12566464, and this gate would be measuring the \
         wrong claim"
    );
    assert!(
        renderer.font_attached(),
        "without the vanilla font the labels draw in the 5x7 debug font, whose \
         advances differ — and the typeface is half of what issue #370 reported"
    );

    for (name, menu, title) in [
        ("chest", Menu::generic(27), "Bob's Loot"),
        ("player inventory", Menu::player(), "Crafting"),
    ] {
        let anchors = label_layout(&menu, &slot_layout(&menu));
        let origin = panel_origin(&slot_layout(&menu), W, H);
        let f = frame(&menu, title, "Inventory");

        let acquired = target.acquire().expect("headless acquire");
        clear_view(device, queue, acquired.view());
        renderer.render(device, queue, acquired.view(), &f, W, H);
        let shot = target.read_texels(device, queue);

        // Reference for "what else already paints here": the same screen with
        // both labels blanked. Any pixel that differs is label ink, by
        // construction, rather than by an assumption about the art underneath.
        let blank = frame(&menu, "", "");
        let acquired = target.acquire().expect("headless acquire");
        clear_view(device, queue, acquired.view());
        renderer.render(device, queue, acquired.view(), &blank, W, H);
        let reference = target.read_texels(device, queue);

        eprintln!("=== {name}: label pixels ===");
        let title_box = changed_bbox(&shot, &reference, origin, [anchors.title_y - 2.0, anchors.title_y + 12.0])
            .unwrap_or_else(|| panic!("{name}: the title drew no pixels at all"));
        assert_at_anchor("title", title_box, [anchors.title_x, anchors.title_y]);

        // The ink colour, and the absence of a shadow, in one measurement: every
        // changed pixel in the title band must be at least as bright as
        // `0x404040`. A drop shadow would put `0x101010` pixels alongside the ink
        // and this fails on them.
        let darkest = darkest_changed(&shot, &reference, origin, [anchors.title_y - 2.0, anchors.title_y + 12.0]);
        eprintln!("  darkest changed pixel in the title band = {darkest:?}");
        assert!(
            darkest.iter().zip(VANILLA_LABEL_RGB).all(|(&got, want)| got + 12 >= want),
            "{name}: vanilla draws these labels in -12566464 == 0xFF404040 with \
             shadow=false (AbstractContainerScreen.java:190-191). The darkest pixel \
             the labels added is {darkest:?}, darker than 0x404040 — which is what a \
             drop shadow (25% of the ink = 0x101010) looks like."
        );

        match anchors.inventory {
            Some([lx, ly]) => {
                let b = changed_bbox(&shot, &reference, origin, [ly - 2.0, ly + 12.0])
                    .unwrap_or_else(|| panic!("{name}: the second label drew no pixels"));
                assert_at_anchor("second label", b, [lx, ly]);
            }
            None => {
                // The same band a chest screen's label occupies, on the screen
                // that omits it. The control is the `Some` arm above, which runs
                // in this same loop against the chest and must find pixels there.
                let band = [166.0 - 94.0 - 2.0, 166.0 - 94.0 + 12.0];
                let b = changed_bbox(&shot, &reference, origin, band);
                eprintln!("  second-label band on the inventory screen = {b:?}");
                assert!(
                    b.is_none(),
                    "{name}: InventoryScreen.extractLabels draws only the title, so \
                     nothing may change in the band a chest's second label occupies; \
                     found ink at {b:?}"
                );
            }
        }
    }
}

/// Bounding box, in local widget pixels, of every pixel that differs between
/// `shot` and `reference` within the horizontal band `[y0, y1)` of the panel.
fn changed_bbox(
    shot: &[u8],
    reference: &[u8],
    origin: (f32, f32),
    band: [f32; 2],
) -> Option<Bbox> {
    let points = changed_points(shot, reference, origin, band);
    bbox_of(&points)
}

fn changed_points(
    shot: &[u8],
    reference: &[u8],
    origin: (f32, f32),
    band: [f32; 2],
) -> Vec<(f32, f32)> {
    let s = scale();
    let y_lo = ((origin.1 + band[0]) * s).max(0.0) as u32;
    let y_hi = (((origin.1 + band[1]) * s) as u32).min(H);
    let x_lo = (origin.0 * s).max(0.0) as u32;
    let x_hi = (((origin.0 + 176.0) * s) as u32).min(W);
    let mut out = Vec::new();
    for y in y_lo..y_hi {
        for x in x_lo..x_hi {
            let i = ((y * W + x) * 4) as usize;
            if shot[i..i + 3] != reference[i..i + 3] {
                out.push((x as f32 / s - origin.0, y as f32 / s - origin.1));
            }
        }
    }
    out
}

/// The darkest per-channel value among the pixels the labels *added*.
fn darkest_changed(shot: &[u8], reference: &[u8], origin: (f32, f32), band: [f32; 2]) -> [u8; 3] {
    let s = scale();
    let y_lo = ((origin.1 + band[0]) * s).max(0.0) as u32;
    let y_hi = (((origin.1 + band[1]) * s) as u32).min(H);
    let x_lo = (origin.0 * s).max(0.0) as u32;
    let x_hi = (((origin.0 + 176.0) * s) as u32).min(W);
    let mut darkest = [255u8; 3];
    for y in y_lo..y_hi {
        for x in x_lo..x_hi {
            let i = ((y * W + x) * 4) as usize;
            if shot[i..i + 3] == reference[i..i + 3] {
                continue;
            }
            let lum = |p: &[u8]| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]);
            if lum(&shot[i..i + 3]) < lum(&darkest) {
                darkest = [shot[i], shot[i + 1], shot[i + 2]];
            }
        }
    }
    darkest
}

fn clear_view(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("label-gate-clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("label-gate-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    queue.submit(std::iter::once(encoder.finish()));
}
