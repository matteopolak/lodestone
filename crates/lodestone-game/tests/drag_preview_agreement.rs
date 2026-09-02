//! The drag preview must show what the release will actually do.
//!
//! `Menu::quick_craft_plan` is the split arithmetic. The container screen draws
//! its per-cell preview numbers from it, and `finish_quick_craft` distributes the
//! cursor with it. This file asserts the two agree — not by comparing two copies
//! of a formula, but by comparing the **plan against the menu afterwards**:
//!
//! ```text
//! plan  = menu.quick_craft_plan(painted, kind, cursor)   // what the screen draws
//! menu.perform_drag(kind, painted, ctx)                  // the real packet sequence
//! after = menu.slot_item(cell).count()                   // what actually happened
//! assert plan[cell].count == after
//! ```
//!
//! `perform_drag` sends the real START / ADD… / END clicks through `do_click`, so
//! the right-hand side is the production path end to end, not a second call to the
//! function under test.
//!
//! # Why this is the discriminator the issue asked for
//!
//! "A number drew in each painted cell" passes on a *wrong* split, which is the
//! failure that matters: a preview that disagrees with the outcome is worse than
//! no preview. So every assertion here is an equality against the outcome, across
//! 2-, 3- and 5-cell drags for both buttons, and the expected values are also
//! stated as literals hand-derived from
//! `AbstractContainerMenu.getQuickCraftPlaceCount` (`:733-740`) — because plan and
//! outcome now share code, agreement alone could in principle be two symmetric
//! misunderstandings (`CLAUDE.md`'s `decode(encode(x)) == x`). The literals are
//! what rule that out.
//!
//! | `kind` | per-cell share |
//! | --- | --- |
//! | `EVEN` (0) | `floor(count / cells)` |
//! | `ONE` (1) | `1` |
//! | `CLONE` (2) | `maxStackSize` |
//!
//! plus, per cell, `+ existing` then `min(maxStackSize, slot cap)`.

use lodestone_game::click::{PlayerCtx, drag_type};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;

fn stack(name: &str, count: i32) -> ItemStack {
    ItemStack::new(name.parse().expect("valid item id"), count)
}

/// A 16-cap item, so the clamp arm (and its yellow-count flag) is reachable with
/// small numbers.
fn stack16(name: &str, count: i32) -> ItemStack {
    ItemStack::new(name.parse().expect("valid item id"), count).with_max_stack_size(16)
}

/// Runs one drag both ways and returns `(previewed, actual)` per painted cell,
/// plus `(previewed remainder, actual cursor)`.
///
/// The preview is read **before** the drag is performed, which is the whole
/// point: it is the number a player would have been looking at.
#[allow(clippy::type_complexity)]
fn preview_then_release(
    mut menu: Menu,
    kind: i32,
    painted: &[usize],
    ctx: PlayerCtx,
) -> (Vec<(usize, i32, i32)>, (i32, i32)) {
    let source = menu.carried().cloned().expect("a drag needs a loaded cursor");
    let plan = menu.quick_craft_plan(painted, kind, &source);
    let remainder = menu.quick_craft_remainder(painted, kind, &source);

    menu.perform_drag(kind, painted, ctx);

    let cells = plan
        .iter()
        .map(|cell| {
            (
                cell.menu_index,
                cell.count,
                menu.slot_item(cell.menu_index).map_or(0, ItemStack::count),
            )
        })
        .collect();
    let cursor = menu.carried().map_or(0, ItemStack::count);
    (cells, (remainder, cursor))
}

/// The per-cell equality — the assertion the issue's discriminator names. The
/// cursor is checked separately by each caller, because `CLONE` is the one drag
/// type where vanilla itself does not make the cursor agree (see
/// [`the_clone_drag_previews_full_stacks_and_a_full_cursor`]).
fn assert_cells_agree(label: &str, cells: &[(usize, i32, i32)]) {
    assert!(
        !cells.is_empty(),
        "[{label}] no cells were previewed at all, so this comparison is vacuous"
    );
    for &(index, previewed, actual) in cells {
        assert_eq!(
            previewed, actual,
            "[{label}] cell {index}: the screen previewed {previewed} and the release \
             produced {actual}. A preview that disagrees with the outcome is worse than \
             no preview — see `Menu::quick_craft_plan`."
        );
    }
}

fn assert_agrees(label: &str, cells: &[(usize, i32, i32)], cursor: (i32, i32)) {
    assert_cells_agree(label, cells);
    assert_eq!(
        cursor.0, cursor.1,
        "[{label}] the cursor: previewed remainder {} vs actual {}",
        cursor.0, cursor.1
    );
}

/// 2, 3 and 5 cells for both buttons, into empty cells. The literals are the
/// hand-derived shares; `assert_agrees` is the equality against the real release.
#[test]
fn the_previewed_split_equals_what_release_produces_for_both_buttons() {
    // (kind, cursor count, painted cells, expected per-cell, expected remainder)
    //
    // EVEN is `floor(count / cells)`: 7/2 = 3 (1 left), 7/3 = 2 (1 left),
    // 7/5 = 1 (2 left). ONE is 1 per cell regardless, so the remainder is
    // `count - cells`.
    let cases: [(i32, i32, &[usize], i32, i32); 6] = [
        (drag_type::EVEN, 7, &[0, 1], 3, 1),
        (drag_type::EVEN, 7, &[0, 1, 2], 2, 1),
        (drag_type::EVEN, 7, &[0, 1, 2, 3, 4], 1, 2),
        (drag_type::ONE, 7, &[0, 1], 1, 5),
        (drag_type::ONE, 7, &[0, 1, 2], 1, 4),
        (drag_type::ONE, 7, &[0, 1, 2, 3, 4], 1, 2),
    ];
    for (kind, count, painted, per_cell, remainder) in cases {
        let label = format!(
            "kind={kind} cursor={count} cells={}",
            painted.len()
        );
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", count)));
        let source = stack("minecraft:stone", count);

        // The hand-derived literals, checked against the plan before anything runs.
        let plan = menu.quick_craft_plan(painted, kind, &source);
        assert_eq!(
            plan.len(),
            painted.len(),
            "[{label}] every painted cell is fillable here, so the plan must cover all of them"
        );
        for cell in &plan {
            assert_eq!(
                cell.count, per_cell,
                "[{label}] cell {} previewed {} where `getQuickCraftPlaceCount` gives {per_cell}",
                cell.menu_index, cell.count
            );
            assert!(!cell.clamped, "[{label}] a 64-cap item into an empty cell is not clamped");
        }
        assert_eq!(
            menu.quick_craft_remainder(painted, kind, &source),
            remainder,
            "[{label}] remainder"
        );

        // …and the equality against the real release path.
        let (cells, cursor) =
            preview_then_release(menu, kind, painted, PlayerCtx::survival());
        assert_agrees(&label, &cells, cursor);
    }
}

/// The same equality with cells that are **already occupied**, so `+ existing`
/// and the per-cell clamp are both exercised. Without this the whole suite above
/// runs against `existing == 0`, where `count` and "amount added" are the same
/// number and a preview that confused the two would pass — `CLAUDE.md`'s *world*
/// species, where the flaw is in the input rather than the assertion.
#[test]
fn the_previewed_split_agrees_over_occupied_and_clamped_cells() {
    let mut menu = Menu::generic(27);
    // A 16-cap item: 12 on the cursor over three cells is 4 each, but cell 1
    // already holds 14, so it clamps to 16 (taking 2, not 4) and cell 2 holds 1
    // so it reaches 5.
    menu.set_carried(Some(stack16("minecraft:egg", 12)));
    menu.set_slot_item(1, Some(stack16("minecraft:egg", 14)));
    menu.set_slot_item(2, Some(stack16("minecraft:egg", 1)));
    let source = stack16("minecraft:egg", 12);
    let painted = [0usize, 1, 2];

    let plan = menu.quick_craft_plan(&painted, drag_type::EVEN, &source);
    let totals: Vec<(usize, i32, bool)> = plan
        .iter()
        .map(|c| (c.menu_index, c.count, c.clamped))
        .collect();
    assert_eq!(
        totals,
        vec![(0, 4, false), (1, 16, true), (2, 5, false)],
        "hand-derived: share = floor(12/3) = 4; cell 1 is 14 + 4 = 18 clamped to the \
         16 cap (and therefore drawn yellow), cell 2 is 1 + 4"
    );
    // Remainder: 12 - (4-0) - (16-14) - (5-1) = 12 - 4 - 2 - 4 = 2.
    assert_eq!(
        menu.quick_craft_remainder(&painted, drag_type::EVEN, &source),
        2
    );

    let (cells, cursor) =
        preview_then_release(menu, drag_type::EVEN, &painted, PlayerCtx::survival());
    assert_agrees("occupied + clamped", &cells, cursor);
}

/// The creative stack-per-slot drag: every painted cell fills to `maxStackSize`,
/// and the previewed cursor is a **full stack regardless of what it
/// distributed** (`recalculateQuickCraftRemaining`, `:251-252`, assigns
/// `maxStackSize` outright rather than subtracting).
///
/// # The one number vanilla does not make agree, and why it is transcribed anyway
///
/// The cursor here is genuinely inconsistent *in vanilla*: `doClick`'s end arm
/// still runs `remaining -= newCount - carry` per cell (`:387`), so a cursor of 3
/// over five 16-cap cells leaves `remaining = 3 - 80 = -77`, and
/// `setCount(-77)` is `isEmpty()`. So the preview shows a full stack and the
/// release empties the cursor.
///
/// This is `CLAUDE.md`'s "three vanilla quirks are transcribed on purpose because
/// they read as bugs — do not fix them", one layer out: making the two agree here
/// would mean either previewing an empty cursor (wrong — vanilla shows a stack) or
/// changing the distribution (a desync). It costs nothing in practice because
/// `CLONE` requires infinite materials. The **per-cell** counts, which is what the
/// issue's discriminator is about, do agree.
#[test]
fn the_clone_drag_previews_full_stacks_and_a_full_cursor() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack16("minecraft:egg", 3)));
    let source = stack16("minecraft:egg", 3);
    let painted = [0usize, 1, 2, 3, 4];

    let plan = menu.quick_craft_plan(&painted, drag_type::CLONE, &source);
    assert_eq!(plan.len(), 5);
    for cell in &plan {
        assert_eq!(cell.count, 16, "CLONE places a whole stack per cell");
    }
    assert_eq!(
        menu.quick_craft_remainder(&painted, drag_type::CLONE, &source),
        16,
        "vanilla sets the remainder to maxStackSize outright, not to a difference"
    );

    let (cells, cursor) =
        preview_then_release(menu, drag_type::CLONE, &painted, PlayerCtx::creative());
    assert_cells_agree("clone drag", &cells);
    assert_eq!(
        cursor,
        (16, 0),
        "vanilla's own divergence, transcribed: the preview shows a full stack while \
         the release's `remaining` goes negative and empties the cursor"
    );
}

/// Control: the plan is **selective**, so an agreement that held because the plan
/// were empty would be vacuous. A cell holding a different item is refused by
/// `can_drag_place_end`, is absent from the plan, and the release leaves it alone
/// — the preview draws no number there, which is the honest answer.
///
/// # The set handed to `quick_craft_plan` is the *filtered* one
///
/// This test originally painted `[0, 1, 2]` with a mismatched item in cell 1 and
/// failed at `previewed 2 vs produced 3`, which looked like the exact defect the
/// file exists to catch. It was the test that was wrong, and the failure is worth
/// keeping written down because the reasoning generalises.
///
/// `painted.len()` is the even-split divisor — vanilla's
/// `getQuickCraftPlaceCount(this.quickcraftSlots.size(), …)`. The machine's set is
/// whatever survived `ADD`, so with cell 1 refused the release divided 6 by **2**
/// and put 3 in each surviving cell, while the plan divided by the 3 cells it was
/// handed. That is not a formula disagreement: the two were given different sets.
///
/// In production they cannot be, because `MenuInput::dragged` applies the same
/// three conditions `can_drag_place` does, so the screen never paints a cell the
/// machine would refuse. So this passes the *filtered* set,
/// which is what `drag_paint` actually returns, and asserts the refusal at the
/// paint boundary instead.
#[test]
fn control_the_plan_omits_a_cell_the_release_would_refuse() {
    let mut menu = Menu::generic(27);
    menu.set_carried(Some(stack("minecraft:stone", 6)));
    menu.set_slot_item(1, Some(stack("minecraft:dirt", 1)));
    let source = stack("minecraft:stone", 6);

    // What the pointer crossed, run through the one predicate both the screen and
    // the machine paint with. Cell 1 is refused, so this is *not* what either side
    // ends up with — asserted, not assumed.
    let mut painted: Vec<usize> = Vec::new();
    for i in [0usize, 1, 2] {
        if menu.can_drag_place_at(i, &source, drag_type::EVEN, painted.len() as i32) {
            painted.push(i);
        }
    }
    assert_eq!(
        painted,
        vec![0, 2],
        "the dirt cell must be refused, or this control measures nothing"
    );

    // 6 over the two surviving cells is 3 each, hand-derived from
    // `getQuickCraftPlaceCount`'s `floor(6 / 2)`.
    let plan = menu.quick_craft_plan(&painted, drag_type::EVEN, &source);
    assert!(
        plan.iter().all(|c| c.count == 3),
        "the divisor is the surviving set's size: {plan:?}"
    );

    let (cells, cursor) =
        preview_then_release(menu, drag_type::EVEN, &painted, PlayerCtx::survival());
    assert_agrees("refused cell", &cells, cursor);
    assert_eq!(cursor.1, 0, "6 - 3 - 3 = 0, so the cursor empties");
}
