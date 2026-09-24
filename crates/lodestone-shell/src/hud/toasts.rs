//! HUD toast views and their top-right draw pass.
//!
//! Toast views are assembled by the application layer; this module owns their
//! shared geometry and rendering.  The view types and layout helper are
//! re-exported by [`crate::hud`] so existing callers keep their API.

use super::{
    Builder, AdvancementToastView, FriendsToastView, RecipeToastView, ADVANCEMENT_TOAST_SPRITE,
    FRIENDS_TOAST_SPRITE, RECIPE_TOAST_DESCRIPTION, RECIPE_TOAST_SPRITE, RECIPE_TOAST_TITLE,
    recipe_toast_rect,
};

/// Draw one recipe-unlock toast.
pub(super) fn draw_recipe_toast(b: &mut Builder<'_>, toast: &RecipeToastView) {
    let (tx, ty, tw, th) = recipe_toast_rect(b.w, toast.visible_portion);
    let quads = b.gui_geometry(RECIPE_TOAST_SPRITE, tx, ty, tw, th);
    if quads.is_empty() {
        b.rect_px(tx, ty, tw, th, [0.86, 0.86, 0.86, 1.0]);
    } else {
        for q in quads {
            b.push_sprite_quad(q, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    b.text(
        RECIPE_TOAST_TITLE,
        tx + 30.0,
        ty + 7.0,
        1.0,
        [0x50 as f32 / 255.0, 0.0, 0x50 as f32 / 255.0, 1.0],
    );
    b.text(
        RECIPE_TOAST_DESCRIPTION,
        tx + 30.0,
        ty + 18.0,
        1.0,
        [0.0, 0.0, 0.0, 1.0],
    );

    let icon = 16.0;
    let station_scale = 0.6;
    b.item_icon(
        &toast.station,
        tx + 3.0 * station_scale,
        ty + 3.0 * station_scale,
        icon * station_scale,
    );
    b.item_icon(&toast.unlocked, tx + 8.0, ty + 8.0, icon);
}

/// Draw one advancement-completion toast.
pub(super) fn draw_advancement_toast(b: &mut Builder<'_>, toast: &AdvancementToastView) {
    let (tx, ty, tw, th) = recipe_toast_rect(b.w, toast.visible_portion);
    let quads = b.gui_geometry(ADVANCEMENT_TOAST_SPRITE, tx, ty, tw, th);
    if quads.is_empty() {
        b.rect_px(tx, ty, tw, th, [0.05, 0.05, 0.08, 0.94]);
    } else {
        for q in quads {
            b.push_sprite_quad(q, [1.0, 1.0, 1.0, 1.0]);
        }
    }
    b.text(&toast.heading, tx + 30.0, ty + 7.0, 1.0, toast.heading_colour);
    b.text(&toast.title, tx + 30.0, ty + 18.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
    if let Some(icon) = &toast.icon {
        b.item_icon(icon, tx + 8.0, ty + 8.0, 16.0);
    }
}

/// Draw one Friends-service notification.
pub(super) fn draw_friends_toast(b: &mut Builder<'_>, toast: &FriendsToastView) {
    let (tx, ty, tw, _) = recipe_toast_rect(b.w, toast.visible_portion);
    let lines = b.wrap_legacy(&toast.message, tw - 7.0 - 4.0, 1.0);
    let content_h = lines.len().max(2) as f32 * 11.0;
    let th = 7.0 + content_h + 3.0;
    let quads = b.gui_geometry(FRIENDS_TOAST_SPRITE, tx, ty, tw, th);
    if quads.is_empty() {
        b.rect_px(tx, ty, tw, th, [0.05, 0.05, 0.08, 0.94]);
    } else {
        for quad in quads {
            b.push_sprite_quad(quad, [1.0, 1.0, 1.0, 1.0]);
        }
    }
    let text_y = ty + 7.0 + (content_h - lines.len() as f32 * 11.0) * 0.5;
    for (index, line) in lines.iter().enumerate() {
        b.text(
            line,
            tx + 7.0,
            text_y + index as f32 * 11.0,
            1.0,
            [1.0, 1.0, 1.0, 1.0],
        );
    }
}
