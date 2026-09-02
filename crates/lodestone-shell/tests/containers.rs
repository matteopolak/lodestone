//! Consolidated test binary for the **containers** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "containers/container_background_pixels.rs"]
mod container_background_pixels;
#[path = "containers/container_cursor_pixels.rs"]
mod container_cursor_pixels;
#[path = "containers/container_drag_preview_pixels.rs"]
mod container_drag_preview_pixels;
#[path = "containers/container_item_pixels.rs"]
mod container_item_pixels;
#[path = "containers/container_item_pixels_scaled.rs"]
mod container_item_pixels_scaled;
#[path = "containers/container_labels.rs"]
mod container_labels;
#[path = "containers/container_screen.rs"]
mod container_screen;
#[path = "containers/container_slot_sprites.rs"]
mod container_slot_sprites;
#[path = "containers/container_special_layout_pixels.rs"]
mod container_special_layout_pixels;
