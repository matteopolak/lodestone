//! The count the **HUD hotbar draws** must change when `Q` throws an
//! item out.
//!
//! # Why this test exists separately from the model tests
//!
//! `lodestone-game/tests/drop_selected_prediction.rs` proves the port is right.
//! It cannot prove the fix reaches a pixel, and this repo's dominant defect is
//! exactly that gap — nine-plus confirmed islands where a subsystem was built,
//! unit-tested green, and consumed by nothing. A model test is a closed loop by
//! construction.
//!
//! So this drives the whole chain the player's report travels, from the model out
//! to the vertex buffer the GPU is handed:
//!
//! ```text
//! Menus::drop_selected            (the prediction)
//!   -> Menus::player()            (== Sim::player_menu())
//!   -> player_native(0..9)        (WindowApp::redraw, transcribed verbatim below)
//!   -> HudFrame::hotbar_items
//!   -> HudGeometry::build         -> geo.verts, the colour stream
//!   -> draw_item_icon_counted     `if slot.count > 1 { … sink.colour.text(&s, …) }`
//! ```
//!
//! `geo.verts` is not a proxy for the drawn frame — it *is* the buffer
//! `HudRenderer` uploads and draws. Building it needs no GPU, no adapter and no
//! atlas, so this is an ordinary fast test rather than an `#[ignore]`d gate.
//!
//! # What is asserted, and why it is not a vertex count
//!
//! A count is a terrible discriminator here: `"5"` and `"4"` are both one glyph,
//! so they emit the *same number* of vertices. Asserting `vertex_count()` changed
//! would pass for a stack that vanished entirely and fail for the correct fix.
//!
//! Instead every case asserts **exact equality of the whole colour stream against
//! an independently-constructed expected frame**, and **inequality against the
//! stale-count frame**:
//!
//! | | |
//! |---|---|
//! | predicted | drop one from 5, then draw |
//! | expected | draw a slot independently seeded with 4 |
//! | control | draw the un-predicted slot, still 5 — the shipped bug |
//!
//! `predicted == expected` is the *magnitude* claim `CLAUDE.md` demands: the frame
//! lands on the value the jar arithmetic predicts, not merely somewhere lower.
//! `predicted != control` is the negative control, and it is load-bearing —
//! without it, `predicted == expected` would also pass if `HudGeometry` ignored
//! the count entirely.
//!
//! # What else paints here, asked before the controls were believed
//!
//! A default `HudFrame` already draws a crosshair and the F3 overlay, and both
//! are identical across all three frames, so they cancel in the comparison but
//! would *mask* nothing: an equality on the full stream cannot be satisfied by
//! unrelated ink the way a percentage could.
//! [`the_hotbar_count_is_what_differs`] pins that the difference between the
//! `5` frame and the `4` frame is really the stack digits and not some incidental
//! layout shift, by checking that a frame with the digits suppressed (`count: 1`)
//! is shorter than either.
//!
//! No GUI atlas and no item atlas are attached, so `geo.item_verts` stays empty
//! and the icon art contributes nothing — the digits are the only slot-dependent
//! ink in the stream. That is asserted, not assumed.

use lodestone::hud::{DebugStats, HotbarSlot, HudFrame, HudGeometry};
use lodestone_assets::ResourceLocation;
use lodestone_game::menus::Menus;
use lodestone_model::{ClientEvent, ItemStack as ModelItemStack};

/// Window-0 menu index of the first hotbar cell (`Menu::player` lays `36..=44`
/// as the hotbar). Native hotbar index 0 is what `SelectedSlot(0)` selects.
const MENU_INDEX_HOTBAR_0: usize = 36;
/// The selected hotbar slot these fixtures drop from.
const SELECTED: usize = 0;

const CANVAS_W: u32 = 640;
const CANVAS_H: u32 = 480;

/// A session whose selected hotbar cell holds `count` cobblestone, seeded through
/// the real window-0 `container_set_content` fold — the shape a server sends.
fn session_with(count: u32) -> Menus {
    let mut menus = Menus::new();
    let mut items = vec![None; 46];
    items[MENU_INDEX_HOTBAR_0] = Some(ModelItemStack {
        item: "minecraft:cobblestone".parse().expect("valid id"),
        count,
        components: lodestone_model::ItemComponents::default(),
    });
    menus.apply(&ClientEvent::ContainerContent {
        window_id: 0,
        state_id: 3,
        items,
        carried_item: None,
    });
    menus
}

/// The nine `HotbarSlot` draw records, built by the **same expression**
/// `WindowApp::redraw` uses.
///
/// Transcribed rather than called because `app/redraw.rs` is `pub(crate)` inside
/// the binary crate. That transcription is the one unproven link in this chain and
/// it is deliberately kept to a shape a reader can diff by eye: the `enchanted`
/// flag and the damage components are irrelevant to a stack count and are carried
/// only so the record is the real one.
fn hotbar_records(menus: &Menus) -> Vec<Option<HotbarSlot>> {
    let player_menu = menus.player();
    (0..9)
        .map(|i| {
            player_menu.player_native(i).and_then(|st| {
                let item = ResourceLocation::parse(&st.item().to_string()).ok()?;
                let damage = st
                    .components()
                    .get_int(lodestone_game::item::DAMAGE_COMPONENT)
                    .and_then(|v| u32::try_from(v).ok());
                let max_damage = st
                    .components()
                    .get_int(lodestone_game::item::MAX_DAMAGE_COMPONENT)
                    .and_then(|v| u32::try_from(v).ok());
                Some(HotbarSlot {
                    item,
                    count: st.count().max(0) as u32,
                    damage,
                    max_damage,
                    enchanted: false,
                    dyed_color: None,
                    potion_color: None,
                    banner_patterns: Vec::new(),
                })
            })
        })
        .collect()
}

/// The colour-stream vertices the HUD would upload for `slots`.
///
/// `hotbar: Some(0)` is what `WindowApp::redraw` installs for a world frame, so
/// the selection highlight and the procedural hotbar frame are present exactly as
/// they are in play — the fixture is not a stripped-down special case.
fn colour_stream(slots: &[Option<HotbarSlot>]) -> Vec<f32> {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.hotbar = Some(SELECTED);
    frame.hotbar_items = Some(slots);
    let geo = HudGeometry::build(&frame, CANVAS_W, CANVAS_H);
    assert!(
        geo.item_verts.is_empty(),
        "no item atlas is attached, so the icon art must contribute nothing and the \
         stack digits are the only slot-dependent ink in `verts`"
    );
    geo.verts
}

/// Plain `Q` from five cobblestone: the HUD draws **four**.
#[test]
fn plain_drop_changes_the_drawn_hotbar_count() {
    let mut menus = session_with(5);
    let control = colour_stream(&hotbar_records(&menus));

    menus.drop_selected(SELECTED, false);
    let predicted = colour_stream(&hotbar_records(&menus));

    let expected = colour_stream(&hotbar_records(&session_with(4)));

    assert_eq!(
        predicted, expected,
        "the drawn frame must be exactly the frame a four-count stack draws — the value \
         `Inventory.removeFromSelected` predicts, not merely something lower"
    );
    assert_ne!(
        predicted, control,
        "negative control: the un-predicted frame is what shipped, and it must differ. \
         If these are equal the HUD is not reading the count at all and the equality \
         above proves nothing"
    );
}

/// `Ctrl`+`Q` from five: the number vanishes from the drawn frame, and the record
/// the icon pass forks on becomes **`None`** rather than `Some(count: 0)`.
///
/// # Why the record is asserted here and not only the stream
///
/// Measured while writing this: with no item atlas attached, a **one**-item cell
/// and an **empty** cell produce byte-identical colour streams — the digits are
/// suppressed at `count == 1` (`draw_item_icon_counted`) and the icon art lives in
/// `item_verts`, which is empty without an atlas. So the stream alone cannot tell
/// "emptied" from "one left", and an assertion that only compared streams would
/// be the *magnitude* species all over again.
///
/// `hotbar_records()[0]` is the fix for that, and it is not a retreat to the
/// model: it is `HudFrame::hotbar_items`' own element type, the value
/// `draw_hotbar_items` matches `if let Some(item) = slot` on to decide whether to
/// draw an icon at all. A surviving `Some(HotbarSlot { count: 0, .. })` would draw
/// a cobblestone icon with no number in a slot the player just emptied — visible
/// the moment an item atlas is attached, invisible to any stream comparison here.
#[test]
fn ctrl_drop_draws_an_empty_cell() {
    let mut menus = session_with(5);
    let control = colour_stream(&hotbar_records(&menus));

    menus.drop_selected(SELECTED, true);
    let records = hotbar_records(&menus);
    let predicted = colour_stream(&records);

    assert!(
        records[SELECTED].is_none(),
        "the record the icon pass forks on must be `None` — a `Some(count: 0)` would draw \
         an icon with no number in an emptied cell"
    );
    assert_eq!(
        predicted,
        colour_stream(&hotbar_records(&Menus::new())),
        "and the drawn frame must be a never-filled cell's"
    );
    assert_ne!(
        predicted, control,
        "negative control: the stale five-count frame must differ"
    );
}

/// Plain `Q` from a stack of two draws a cell with **no number at all** while the
/// item stays put — vanilla suppresses the digits at `count == 1`
/// (`draw_item_icon_counted`'s `if slot.count > 1`).
///
/// This is the boundary that separates the correct fix from one that *empties* the
/// slot instead of decrementing it: both "lowered the count", and both draw no
/// number, so only the record distinguishes them (see
/// [`ctrl_drop_draws_an_empty_cell`] for why the stream cannot).
#[test]
fn dropping_to_one_removes_the_number_but_keeps_the_item() {
    let mut menus = session_with(2);
    let control = colour_stream(&hotbar_records(&menus));

    menus.drop_selected(SELECTED, false);
    let records = hotbar_records(&menus);
    let predicted = colour_stream(&records);

    assert_eq!(
        records[SELECTED].as_ref().map(|s| s.count),
        Some(1),
        "the cell is still occupied, by exactly one — this is the assertion a fix that \
         emptied the slot would fail, and the only one that can see the difference \
         without an item atlas"
    );
    assert_eq!(
        predicted,
        colour_stream(&hotbar_records(&session_with(1))),
        "…and it draws as a one-count cell: icon, no number"
    );
    assert_ne!(
        predicted, control,
        "negative control: the stale two-count frame drew a '2' and must differ"
    );
}

/// The premise every comparison above rests on: the stack digits are really what
/// differs between these frames.
///
/// A control's premise can be false in the safe-looking direction, so the numbers
/// here are measured rather than assumed — and the first assumption made while
/// writing this was **wrong** in a way worth keeping. `"5"` and `"4"` do *not*
/// emit the same number of vertices: the procedural glyph path is stroke-based, so
/// a `5` costs six quads more than a `4` (101,016 floats against 100,800, a
/// 216-float = 36-vertex = 6-quad gap). Both facts below therefore hold — the
/// streams differ in length *and* in content — and the tests above compare content
/// so they would still catch a same-length wrong digit.
#[test]
fn the_hotbar_count_is_what_differs() {
    let five = colour_stream(&hotbar_records(&session_with(5)));
    let four = colour_stream(&hotbar_records(&session_with(4)));
    let one = colour_stream(&hotbar_records(&session_with(1)));

    assert!(
        one.len() < four.len() && one.len() < five.len(),
        "a single item draws no number, so its stream must be strictly shorter than \
         either digit-bearing frame: one = {}, four = {}, five = {}. If these were equal \
         the digits are not in `verts` at all and every comparison above is measuring \
         incidental ink",
        one.len(),
        four.len(),
        five.len()
    );
    assert_ne!(
        five, four,
        "and a '5' frame must differ from a '4' frame — the discriminator the reported \
         bug turns on"
    );
}
