//! Consolidated test binary for the **text** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "text/command_message_translation.rs"]
mod command_message_translation;
#[path = "text/sign_text_distance_stability_pixels.rs"]
mod sign_text_distance_stability_pixels;
#[path = "text/sign_text_pixels.rs"]
mod sign_text_pixels;
#[path = "text/text_colour.rs"]
mod text_colour;
#[path = "text/text_display_pixels.rs"]
mod text_display_pixels;
#[path = "text/vanilla_font_pixels.rs"]
mod vanilla_font_pixels;
#[path = "text/world_text_gamma_blend_pixels.rs"]
mod world_text_gamma_blend_pixels;
#[path = "text/world_text_over_geometry_pixels.rs"]
mod world_text_over_geometry_pixels;
