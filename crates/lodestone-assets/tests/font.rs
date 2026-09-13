//! Consolidated test binary for the **font** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "font/font.rs"]
mod font;
#[path = "font/font_ttf.rs"]
mod font_ttf;
#[path = "font/font_unihex.rs"]
mod font_unihex;
#[path = "font/unihex_vanilla_oracle.rs"]
mod unihex_vanilla_oracle;
#[path = "font/vanilla_font_metrics.rs"]
mod vanilla_font_metrics;
