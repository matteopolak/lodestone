//! Consolidated test binary for the **hud** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "hud/advancements_hover_dim_pixels.rs"]
mod advancements_hover_dim_pixels;
#[path = "hud/air_bubble_pixels.rs"]
mod air_bubble_pixels;
#[path = "hud/attack_indicator_pixels.rs"]
mod attack_indicator_pixels;
#[path = "hud/block_outline_thickness_pixels.rs"]
mod block_outline_thickness_pixels;
#[path = "hud/chat_input_gap.rs"]
mod chat_input_gap;
#[path = "hud/chat_scrollbar_paint.rs"]
mod chat_scrollbar_paint;
#[path = "hud/debug_line_f3_overlay.rs"]
mod debug_line_f3_overlay;
#[path = "hud/debug_line_ribbon_width_pixels.rs"]
mod debug_line_ribbon_width_pixels;
#[path = "hud/debug_overlay_lines_fit_the_canvas.rs"]
mod debug_overlay_lines_fit_the_canvas;
#[path = "hud/held_item_name_pixels.rs"]
mod held_item_name_pixels;
#[path = "hud/hotbar_block_item_pixels.rs"]
mod hotbar_block_item_pixels;
#[path = "hud/hotbar_drop_prediction_pixels.rs"]
mod hotbar_drop_prediction_pixels;
#[path = "hud/hotbar_special_item_pixels.rs"]
mod hotbar_special_item_pixels;
#[path = "hud/hud_text_scale.rs"]
mod hud_text_scale;
#[path = "hud/hurt_overlay_pixels.rs"]
mod hurt_overlay_pixels;
#[path = "hud/legacy_codes_never_reach_a_glyph.rs"]
mod legacy_codes_never_reach_a_glyph;
#[path = "hud/menu_button_pixels.rs"]
mod menu_button_pixels;
#[path = "hud/menu_panorama_pixels.rs"]
mod menu_panorama_pixels;
#[path = "hud/screen_overlay_pixels.rs"]
mod screen_overlay_pixels;
#[path = "hud/stack_count_anchor_pixels.rs"]
mod stack_count_anchor_pixels;
#[path = "hud/view_bob_pixels.rs"]
mod view_bob_pixels;
