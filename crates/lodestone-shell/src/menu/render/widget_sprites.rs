use super::*


#[test]
fn the_button_sprite_matches_vanillas_enabled_hovered_rule() {
    // `WidgetSprites::get(enabled, focused)` with vanilla's own abstract
    // button base's three-argument sprite selection (its own source, and its
    // own widget-sprites companion): enabled+hovered → highlighted,
    // enabled → button, and **disabled wins over hovered** → disabled.
    //
    // The assertion is on *which atlas region the UVs sample*, not on "a
    // quad appeared" — the three states all cover the same pixels, so
    // presence alone cannot tell them apart.
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let one = |enabled: bool, selected: bool| {
        let rows = vec![MenuRow {
            label: "Options...".into(),
            enabled,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: -100.0,
                dy: 40.0,
                w: 200.0,
                h: 20.0,
            }),
            ..Default::default()
        }];
        let mut f = frame_with(rows, if selected { 0 } else { 99 });
        f.vanilla = true;
        build(&f, Some(&atlas), None, V_W, V_H).sprite
    };

    let plain = sprite_uv_bounds(&atlas, "widget/button");
    let hover = sprite_uv_bounds(&atlas, "widget/button_highlighted");
    let off = sprite_uv_bounds(&atlas, "widget/button_disabled");
    // The three regions must be disjoint, or "sampled inside X" proves
    // nothing. Different sizes are not enough; check the packer actually
    // separated them.
    for (a, b) in [(plain, hover), (plain, off), (hover, off)] {
        assert!(
            a.1[0] <= b.0[0] || b.1[0] <= a.0[0] || a.1[1] <= b.0[1] || b.1[1] <= a.0[1],
            "two button sprites share atlas space: {a:?} {b:?}"
        );
    }

    assert!(
        all_uvs_within(&one(true, false), plain.0, plain.1),
        "an idle enabled button must sample widget/button"
    );
    assert!(
        all_uvs_within(&one(true, true), hover.0, hover.1),
        "a hovered enabled button must sample widget/button_highlighted"
    );
    assert!(
        all_uvs_within(&one(false, true), off.0, off.1),
        "a hovered DISABLED button must still sample widget/button_disabled"
    );
    // The control that makes the last one a real measurement: the same
    // hovered flag on an *enabled* button does not sample the disabled
    // sprite, so the assertion is not passing because everything does.
    assert!(
        !all_uvs_within(&one(true, true), off.0, off.1),
        "the detector cannot tell the disabled sprite apart"
    );
    // And with no atlas there is no sprite stream at all — the jar-less
    // path, which is why the flat-fill fallback exists.
    let rows = vec![MenuRow {
        label: "Options...".into(),
        enabled: true,
        slot: Some(Slot {
            origin: Origin::ScreenTop,
            dx: -100.0,
            dy: 40.0,
            w: 200.0,
            h: 20.0,
        }),
        ..Default::default()
    }];
    let mut f = frame_with(rows, 0);
    f.vanilla = true;
    let bare = build(&f, None, None, V_W, V_H);
    assert!(bare.sprite.is_empty(), "no atlas must mean no sprite quads");
    assert!(
        bare.colour.len() > bare.backdrop_floats,
        "and the flat fallback must still draw the button"
    );
}

#[test]
fn every_title_and_pause_widget_draws_the_sprite_the_widget_layer_picks() {
    use crate::menu::nav::{MAIN_BUTTONS, PAUSE_BUTTONS};

    // The dead-code path this rules out is the one a widget-layer wiring error could
    // create: `menu/widget.rs` compiles, its own tests are green, and
    // `draw_widget` keeps a private three-way `if` — so the widget layer is
    // dead code while every existing gate still passes.
    //
    // The expected sprite here is produced by `WidgetSprites::get`
    // (`menu::widget`), never spelled out, and the measurement is *which
    // atlas region the frame's own UVs sample*. So a `draw_widget` that
    // stopped consulting the widget would have to keep agreeing with
    // vanilla's rule by coincidence, for all 36 (button, focused) pairs, to
    // pass — and if the rule in `widget.rs` is wrong, this fails too.
    // The arranged layout extends it in the other direction, without new
    // machinery: each
    // case is now drawn at that button's **own** slot, and the sprite's
    // destination rect is asserted against it. `title_slot`/`pause_slot` read
    // the arranged layout tree, so this is also the gate that says
    // the layout containers reach pixels — an arrange pass that silently
    // no-opped would put every widget at the block's origin and fail here
    // while every "a button drew something" check still passed.
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    // Both real screens' real button states and real rects, labelled so a
    // failure names the button rather than an index. `icon: None` throughout:
    // the synthetic pack carries one icon sprite, and an icon quad would put a
    // second region in the stream and make `all_uvs_within` a weaker question
    // (it would not disturb `sprite_dest_bounds`, which the icon sits inside).
    let cases: Vec<(&'static str, bool, Slot)> = MAIN_BUTTONS
        .iter()
        .map(|b| (b.label(), b.enabled(), title_slot(*b)))
        .chain(
            PAUSE_BUTTONS
                .iter()
                .map(|b| (b.label(), b.enabled(), pause_slot(*b, false))),
        )
        .collect();
    // The premise, checked rather than assumed: both screens really do carry
    // a mix, or "the disabled sprite was chosen" is never exercised.
    assert!(
        cases.iter().any(|(_, e, _)| *e) && cases.iter().any(|(_, e, _)| !*e),
        "neither screen has a disabled button any more, so this gate is vacuous"
    );
    // And the rects are really distinct, or the position half of this gate is
    // satisfied by every widget landing in one place.
    let distinct: std::collections::BTreeSet<(i32, i32)> = cases
        .iter()
        .map(|(_, _, s)| {
            let (x, y, ..) = s.resolve(V_W, V_H);
            (x as i32, y as i32)
        })
        .collect();
    assert_eq!(
        distinct.len(),
        cases.len(),
        "two buttons share a position, so a widget stuck at the wrong one \
         could still pass"
    );

    for (label, enabled, slot) in cases {
        for focused in [false, true] {
            let rows = vec![MenuRow {
                label: label.to_string(),
                enabled,
                slot: Some(slot),
                ..Default::default()
            }];
            let mut f = frame_with(rows, if focused { 0 } else { 99 });
            f.vanilla = true;
            let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;

            let expected = widget::BUTTON_SPRITES.get(enabled, focused);
            let (min, max) = sprite_uv_bounds(&atlas, expected);
            assert!(
                all_uvs_within(&sprite, min, max),
                "{label} (enabled={enabled}, focused={focused}) did not sample \
                 {expected}, which is what WidgetSprites::get selects"
            );
            // The control for each case: flipping `active` must move the
            // sample off this region, so "inside {expected}" is a real
            // discriminator and not something every render satisfies.
            let flipped = widget::BUTTON_SPRITES.get(!enabled, focused);
            if flipped != expected {
                let (fmin, fmax) = sprite_uv_bounds(&atlas, flipped);
                assert!(
                    !all_uvs_within(&sprite, fmin, fmax),
                    "the detector cannot tell {expected} from {flipped}"
                );
            }

            // Where it drew, in logical pixels, against the layout's own
            // answer for this button. The 0.01 is the NDC round trip's float
            // error, not slack in the layout — see `sprite_dest_bounds`.
            let drawn = sprite_dest_bounds(&sprite, V_W, V_H);
            let want = slot.resolve(V_W, V_H);
            let same = [
                (drawn.0, want.0),
                (drawn.1, want.1),
                (drawn.2, want.2),
                (drawn.3, want.3),
            ]
            .iter()
            .all(|(a, b)| (a - b).abs() < 0.01);
            assert!(
                same,
                "{label} (enabled={enabled}, focused={focused}) drew at {drawn:?}, \
                 not at {want:?} where the layout placed it"
            );
        }
    }
}

#[test]
fn nine_slice_borders_come_from_the_mcmeta_not_a_constant() {
    // `widget/button` declares `border: 3` and `widget/button_disabled`
    // declares `border: 1` in the real 26.2 pack — read straight out of
    // `client.jar`. A renderer that hardcoded one border would draw the
    // disabled button's corners three times too large, which is exactly the
    // subtle wrongness the brief warned about.
    //
    // The synthetic pack repeats those two values, so the corner quad's own
    // destination size is the discriminator.
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let corner_size = |id: &str| {
        // Drawn far wider than native so every nine-slice piece appears.
        let quads = atlas.geometry(id, 0.0, 0.0, 400.0, 60.0);
        assert!(quads.len() >= 9, "{id} did not decompose: {}", quads.len());
        // The top-left piece is the one at the draw origin.
        let tl = quads
            .iter()
            .find(|q| q.dst[0] == 0.0 && q.dst[1] == 0.0)
            .expect("a nine-slice has a top-left corner");
        (tl.dst[2], tl.dst[3])
    };
    assert_eq!(corner_size("widget/button"), (3.0, 3.0));
    assert_eq!(
        corner_size("widget/button_disabled"),
        (1.0, 1.0),
        "the disabled sprite's border must come from its own .mcmeta"
    );
}

#[test]
fn a_disabled_label_is_drawn_in_vanillas_grey_and_an_enabled_one_in_white() {
    // Vanilla's own abstract-widget base's with-inactive-message default
    // recolours
    // an inactive widget's message to `-6250336` == `0xFFA0A0A0`
    //. Assert the actual colour, with the
    // enabled case as the control.
    let slot = Slot {
        origin: Origin::ScreenTop,
        dx: -100.0,
        dy: 40.0,
        w: 200.0,
        h: 20.0,
    };
    let render = |enabled: bool| {
        let rows = vec![MenuRow {
            label: "MMMM".into(),
            enabled,
            slot: Some(slot),
            ..Default::default()
        }];
        let mut f = frame_with(rows, 99);
        f.vanilla = true;
        build(&f, None, None, V_W, V_H).colour
    };
    let (w, h) = (V_W, V_H);
    let (x, y, rw, rh) = slot.resolve(w, h);
    // Sample the label band across the middle of the button.
    let band = (x + rw * 0.3, y + rh * 0.3, rw * 0.4, rh * 0.4);
    let off = render(false);
    let on = render(true);
    assert!(
        coverage_of(&off, w, h, band, widget::INACTIVE_LABEL) > 0.02,
        "no grey label ink in a disabled button's rect: {}",
        coverage_of(&off, w, h, band, widget::INACTIVE_LABEL)
    );
    assert_eq!(
        coverage_of(&off, w, h, band, LABEL),
        0.0,
        "a disabled label must not be drawn in the enabled colour"
    );
    assert!(
        coverage_of(&on, w, h, band, LABEL) > 0.02,
        "no white label ink in an enabled button's rect: {}",
        coverage_of(&on, w, h, band, LABEL)
    );
    assert_eq!(
        coverage_of(&on, w, h, band, widget::INACTIVE_LABEL),
        0.0,
        "an enabled label must not be drawn grey"
    );
    // The colour under test comes from the widget layer, and *that* is
    // checked against vanilla's signed ARGB integer by
    // `widget::tests::vanillas_inactive_grey_is_derived_not_transcribed`
    // rather than being restated here. What this line pins is that the two
    // files still agree: the draw grey is the widget grey.
    assert_eq!(
        widget::INACTIVE_LABEL,
        widget::argb_to_rgba(widget::INACTIVE_MESSAGE_ARGB),
        "vanilla's -6250336 is 0xFFA0A0A0"
    );
}

#[test]
fn an_icon_button_draws_its_sprite_and_no_label() {
    // Vanilla's own sprite-icon-button centred-icon variant draws the button background
    // plus a 15×15 sprite centred in it, and no text
    //.
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let slot = Slot {
        origin: Origin::ScreenTop,
        dx: -10.0,
        dy: 40.0,
        w: 20.0,
        h: 20.0,
    };
    let row = |icon: Option<&'static str>| MenuRow {
        label: "Language...".into(),
        enabled: false,
        slot: Some(slot),
        icon,
        ..Default::default()
    };
    let render = |icon: Option<&'static str>| {
        let mut f = frame_with(vec![row(icon)], 99);
        f.vanilla = true;
        build(&f, Some(&atlas), None, V_W, V_H)
    };

    let icon = render(Some("icon/language"));
    let bare = render(None);
    let icon_uv = sprite_uv_bounds(&atlas, "icon/language");
    assert!(
        any_quad_centre_in(&icon.sprite, icon_uv.0, icon_uv.1),
        "the icon sprite never reached the sprite stream"
    );
    // The control: without the icon, nothing samples that atlas region.
    assert!(
        !any_quad_centre_in(&bare.sprite, icon_uv.0, icon_uv.1),
        "the detector matches the button background too"
    );
    // And it is exactly one extra quad, drawn at the centred 15×15 rect —
    // both variants draw the same nine-slice background.
    assert_eq!(
        icon.sprite.len() - bare.sprite.len(),
        SPRITE_FLOATS_PER_VERTEX * 6,
        "an icon button should add exactly one quad"
    );
    // And an icon button draws no label ink: with the icon set, the only
    // colour quads are the backdrop.
    assert_eq!(
        icon.colour.len(),
        icon.backdrop_floats,
        "an icon button must draw no text"
    );
    assert!(
        bare.colour.len() > bare.backdrop_floats,
        "but the same row *with* a label does draw text"
    );
}

#[test]
fn the_pause_overlays_backdrop_is_vanillas_measured_black_at_alpha_64() {
    // `inworld_menu_background.png` decoded out of the real `client.jar` is
    // 16×16 greyscale+alpha with every pixel grey 0 / alpha 64
    // (vanilla's own screen base tiles it at 32 px). This pins the exact
    // value rather than "translucent enough".
    let nav = test_nav("overlay-exact");
    let v = geometry(&pause_frame(&nav), V_W, V_H);
    assert_eq!(&v[2..6], &[0.0, 0.0, 0.0, 64.0 / 255.0]);
}

/// Vanilla draws `inworld_menu_background.png` as a tiled raw texture, not as
/// a colour constant. The vanilla file happens to be uniform black at alpha
/// 64, which made the old fallback look correct while silently discarding a
/// server pack's screen art.
#[test]
fn a_pack_menu_background_tiles_over_the_pause_screen() {
    use lodestone_assets::{MemorySource, ResourceManager, ResourceSource};

    assert!(
        crate::resources::MENU_TEXTURES
            .iter()
            .any(|entry| *entry == crate::resources::INWORLD_MENU_BACKGROUND_TEXTURE),
        "the production menu atlas must carry the raw in-world background, not \
         merely a synthetic test atlas"
    );
    let mut source = MemorySource::default();
    source.insert(
        crate::resources::INWORLD_MENU_BACKGROUND_TEXTURE.1,
        solid_rgba_png(16, 16, [31, 127, 223, 255]),
    );
    // `GuiAtlas` is not extras-only: one ordinary sprite establishes the
    // normal source walk while the raw texture exercises the screen-background
    // path this gate is about.
    source.insert(
        "assets/minecraft/textures/gui/sprites/icon/language.png",
        solid_rgba_png(15, 15, [0, 0, 0, 255]),
    );
    let manager = ResourceManager::new(vec![Box::new(source) as Box<dyn ResourceSource>]);
    let atlas = GuiAtlas::build_with_extras(
        &manager,
        &[crate::resources::INWORLD_MENU_BACKGROUND_TEXTURE],
    )
    .expect("the synthetic pack background stitches");

    let frame = MenuFrame {
        backdrop: MenuBackdrop::Dim,
        ..Default::default()
    };
    let drawn = build(&frame, Some(&atlas), None, 64.0, 32.0);
    let (min, max) = loose_uv_bounds(&atlas, crate::resources::INWORLD_MENU_BACKGROUND_TEXTURE.0);

    assert!(
        any_quad_centre_in(&drawn.sprite, min, max),
        "the packed in-world menu background never reaches the pause screen; \
         it fell back to the hard-coded overlay colour instead"
    );
    assert_eq!(
        drawn.sprite.len(),
        SPRITE_FLOATS_PER_VERTEX * 6 * 2,
        "a 64x32 screen is exactly two vanilla 32x32 menu-background tiles"
    );
    assert!(
        drawn.colour.is_empty(),
        "a supplied background texture replaces, rather than double-darkens \
         with, the black-alpha fallback"
    );
}

/// BookViewScreen and BookEditScreen blit the loose `gui/book.png` sheet,
/// rather than composing a panel from `gui/sprites/**`. Keep that source in
/// the menu atlas extras so the active resource-pack stack supplies the art.
#[test]
fn book_gui_texture_is_a_menu_atlas_extra() {
    assert!(
        crate::resources::MENU_TEXTURES.iter().any(|(id, path)| {
            *id == "book/background" && *path == "assets/minecraft/textures/gui/book.png"
        }),
        "the book GUI is a raw texture outside gui/sprites and must be loaded as a menu extra"
    );
}

/// The raw book sheet is 256×256 but the vanilla screens sample only its
/// top-left 192×192 window. A whole-sheet blit stretches transparent padding
/// into the page and is visibly unlike the server pack's supplied art.
#[test]
fn book_frames_sample_the_vanilla_192_pixel_region_of_the_pack_texture() {
    use lodestone_assets::{MemorySource, ResourceSource};

    let mut source = MemorySource::default();
    source.insert(
        crate::resources::BOOK_GUI_TEXTURE.1,
        solid_rgba_png(256, 256, [120, 80, 40, 255]),
    );
    // `GuiAtlas` is intentionally not an extras-only stitch: one ordinary GUI
    // sprite establishes the atlas's normal source set, while the book sheet
    // exercises the loose-extra path this test is about.
    source.insert(
        "assets/minecraft/textures/gui/sprites/icon/language.png",
        solid_rgba_png(15, 15, [0, 0, 0, 255]),
    );
    let manager = lodestone_assets::ResourceManager::new(vec![Box::new(source) as Box<dyn ResourceSource>]);
    let atlas = GuiAtlas::build_with_extras(&manager, &[crate::resources::BOOK_GUI_TEXTURE])
        .expect("the synthetic loose book sheet stitches");
    let state = crate::menu::book_view::BookViewState::new(
        crate::menu::book_view::BookViewOpen::from_pages(
            "Notes".to_string(),
            "Steve".to_string(),
            0,
            &[lodestone_model::text::Text::literal("page")],
                                &|_| None,
),
    );
    let drawn = build(&book_view_frame(&state), Some(&atlas), None, V_W, V_H);
    let (min, max) = loose_uv_bounds(&atlas, "book/background");
    let book = ((V_W * 0.5).floor() - 96.0, 2.0, 192.0, 192.0);
    let uvs = uvs_in_dest(&drawn.sprite, V_W, V_H, book);

    assert_eq!(uvs.len(), 6, "one unsliced book-background quad lands at its screen rect");
    let expected_max = [min[0] + (max[0] - min[0]) * 0.75, min[1] + (max[1] - min[1]) * 0.75];
    for &[u, v] in &uvs {
        assert!(u >= min[0] - 1e-6 && u <= expected_max[0] + 1e-6, "u={u} samples outside the 192/256 book window");
        assert!(v >= min[1] - 1e-6 && v <= expected_max[1] + 1e-6, "v={v} samples outside the 192/256 book window");
    }
    assert!(
        uvs.iter().any(|[u, v]| (*u - expected_max[0]).abs() < 1e-6 && (*v - expected_max[1]).abs() < 1e-6),
        "the sampled region reaches the exact 192/256 source edge rather than shrinking the book"
    );
}

/// Book screens pass `false` for Minecraft's per-call text-shadow flag.  The
/// frame-level marker must cover every route through `Quads::text`, including
/// page labels, the indicator, editable text, and ordinary button captions;
/// a red probe makes the omitted shadow distinguishable from black book ink.
#[test]
fn book_frame_text_is_plain_while_other_menu_text_keeps_its_shadow() {
    use lodestone_assets::{MemorySource, ResourceManager, ResourceSource};

    let mut source = MemorySource::new("book-plain-text-font");
    source.insert(
        "assets/minecraft/textures/font/t.png",
        solid_rgba_png(1, 1, [255, 255, 255, 255]),
    );
    source.insert(
        "assets/minecraft/font/default.json",
        br#"{"providers":[{"type":"bitmap","file":"minecraft:font/t.png","ascent":1,"height":1,"chars":["A"]}]}"#.to_vec(),
    );
    let manager = ResourceManager::new(vec![Box::new(source) as Box<dyn ResourceSource>]);
    let font = crate::hud::VanillaFont::from_manager(&manager).expect("synthetic font loads");
    let ink = [0.8, 0.4, 0.2, 1.0];
    let shadow = crate::hud::vanilla_font::shadow_of(ink);
    let frame = |book_background| MenuFrame {
        vanilla: true,
        book_background,
        labels: vec![MenuLabel {
            text: "A".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 0.0,
            align: Align::Left,
            colour: ink,
            scale: 1.0,
        }],
        ..Default::default()
    };
    let has_colour = |geometry: &MenuGeometry, colour: [f32; 4]| {
        geometry.colour.chunks_exact(STRIDE).any(|vertex| {
            (2..6).all(|index| (vertex[index] - colour[index - 2]).abs() < 1e-5)
        })
    };

    let book = build(&frame(true), None, Some(&font), V_W, V_H);
    assert!(has_colour(&book, ink), "control: the book label's main ink drew");
    assert!(
        !has_colour(&book, shadow),
        "book text must not emit the normal offset shadow pass"
    );

    let ordinary = build(&frame(false), None, Some(&font), V_W, V_H);
    assert!(
        has_colour(&ordinary, shadow),
        "control: non-book menus keep their normal shadow pass"
    );
}

#[test]
#[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
fn every_sprite_id_the_vanilla_screens_name_exists_in_the_real_pack() {
    use crate::menu::nav::{MAIN_BUTTONS, PAUSE_BUTTONS};

    // The island this rules out: a mistyped sprite id draws *nothing*, and
    // every layout assertion above still passes because they use a synthetic
    // pack whose ids are the same strings the test itself wrote. Only the
    // real jar can say whether `pause_menu/social_interactions` is spelled
    // right.
    let atlas = crate::resources::load_menu_gui_atlas().expect(
        "no vanilla pack found; set LODESTONE_ASSETS to a root with client.jar",
    );
    // Every id the widget layer can select, taken from the record itself
    // rather than relisted — so a sprite added to `WidgetSprites` is covered
    // here the day it exists.
    let button_ids = [
        widget::BUTTON_SPRITES.enabled,
        widget::BUTTON_SPRITES.disabled,
        widget::BUTTON_SPRITES.enabled_focused,
        widget::BUTTON_SPRITES.disabled_focused,
    ];
    for id in button_ids {
        assert!(atlas.contains(id), "the pack has no {id}");
        assert_eq!(
            atlas.native_size(id),
            Some((200, 20)),
            "{id} is not the 200x20 its .mcmeta declares"
        );
    }
    for icon in MAIN_BUTTONS
        .iter()
        .filter_map(|b| b.icon())
        .chain(PAUSE_BUTTONS.iter().filter_map(|b| b.icon()))
    {
        assert!(atlas.contains(icon), "the pack has no icon sprite {icon}");
        assert!(atlas.native_size(icon).is_some(), "{icon} was not placed");
        // Deliberately *no* assertion on the native size, and this is a
        // belief that was held and measured false. "Vanilla's icon-button
        // sprites are 15×15" is true of every **blit** (the sprite width and
        // height are 15 at each call site — vanilla's own common-buttons,
        // friends-button, and pause-screen rendering) and true
        // of almost none of the **files**. Measured out of the real 26.2 jar:
        //
        //   icon/language 15×15, icon/accessibility 15×15,
        //   friends/friends 16×16, pause_menu/bug 13×13,
        //   pause_menu/social_interactions 20×20,
        //   pause_menu/player_reporting 15×14
        //
        // They are all `Stretch` (no `.mcmeta`), so vanilla scales each to
        // 15×15 — including *up* from 13 and *down* from 20. Two successive
        // versions of this gate asserted a native size and were failed by
        // `friends/friends` and then `pause_menu/bug`. Drawing at
        // [`ICON_SPRITE`] is what matches vanilla; the file size is not
        // something to check against.
    }
    // The two loose title textures, and their *declared* (not native) size:
    // 26.2 ships them at 4x, which is why the draw rect is 256x64 / 128x16.
    assert_eq!(atlas.native_size("title/minecraft"), Some((1024, 256)));
    assert_eq!(atlas.native_size("title/edition"), Some((512, 64)));

    // The Resource Packs screen's own sprites: the two hover overlays
    // off `gui/sprites/transferable_list/**`, plus the *loose*
    // `misc/unknown_pack` fallback icon, which only reaches the atlas because
    // `MENU_TEXTURES` names it. A typo in any of them draws nothing at all, in
    // silence — which is this gate's whole reason.
    for id in [
        super::draw::PACK_SELECT_SPRITES.0,
        super::draw::PACK_SELECT_SPRITES.1,
        super::draw::PACK_UNSELECT_SPRITES.0,
        super::draw::PACK_UNSELECT_SPRITES.1,
        super::draw::PACK_UNKNOWN_ICON,
    ] {
        assert!(atlas.contains(id), "the pack has no {id}");
    }
    assert_eq!(
        atlas.native_size(super::draw::PACK_SELECT_SPRITES.0),
        Some((32, 32)),
        "the overlay sprites are vanilla's own pack-entry icon-size square"
    );

    // The real pack's nine-slice borders, which is where the hardcoding trap
    // is: 3 for button and button_highlighted, **1** for button_disabled.
    let corner = |id: &str| {
        let q = atlas.geometry(id, 0.0, 0.0, 400.0, 60.0);
        let tl = q
            .iter()
            .find(|q| q.dst[0] == 0.0 && q.dst[1] == 0.0)
            .expect("nine-slice top-left");
        (tl.dst[2], tl.dst[3])
    };
    assert_eq!(corner(widget::BUTTON_SPRITES.enabled), (3.0, 3.0));
    assert_eq!(corner(widget::BUTTON_SPRITES.enabled_focused), (3.0, 3.0));
    assert_eq!(corner(widget::BUTTON_SPRITES.disabled), (1.0, 1.0));

    // And the whole title frame draws through it: every sprite the two
    // screens ask for resolves to at least one quad.
    let nav = test_nav("real-pack");
    let mut ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let geo = build(&title, Some(&atlas), None, V_W, V_H);
    // 9 nine-slice backgrounds (the 8 vanilla widgets plus the
    // non-vanilla `Accounts` row — see `MainButton::Accounts`) + 3 icons
    // + 2 logo quads, so comfortably more than one quad per widget, and
    // *nothing* on the flat-fill path.
    assert!(
        geo.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6) > MAIN_BUTTONS.len(),
        "only {} sprite quads for {} widgets plus the logo",
        geo.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6),
        MAIN_BUTTONS.len()
    );
    assert_eq!(
        geo.colour.len(),
        geo.backdrop_floats
            + geometry(&title, V_W, V_H).len()
            - geometry_button_fill_floats(&title, V_W, V_H)
            - geo.backdrop_floats,
        "with a real atlas no button may fall back to a flat fill"
    );

    ui.enter_dev_world();
    ui.pause();
    let pause = build(&pause_frame(&nav), Some(&atlas), None, V_W, V_H);
    assert!(
        pause.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6) > PAUSE_BUTTONS.len(),
        "the pause screen's nine widgets did not all draw a sprite"
    );
}

