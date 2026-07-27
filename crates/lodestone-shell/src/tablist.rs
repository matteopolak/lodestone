//! Tab-list display projection.
//!
//! The authoritative fold lives in [`lodestone_game::tablist::TabList`]. This
//! module only lowers that state into the flat text rows the shell HUD already
//! draws while Tab is held.

use lodestone_game::tablist::TabList;

/// Formats the listed players in vanilla display order as HUD rows.
#[must_use]
pub fn player_rows(tab_list: &TabList) -> Vec<String> {
    tab_list
        .ordered()
        .into_iter()
        .map(|entry| {
            let name = entry.effective_name().to_plain_string();
            if entry.latency >= 0 {
                format!("{name}  {}ms", entry.latency)
            } else {
                format!("{name}  --")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::tablist::{GameProfile, PlayerListEntry};
    use lodestone_model::{GameMode, Text};
    use uuid::Uuid;

    fn entry(id: u128, name: &str, latency: i32, mode: GameMode) -> PlayerListEntry {
        let mut entry = PlayerListEntry::new(GameProfile::new(Uuid::from_u128(id), name));
        entry.latency = latency;
        entry.game_mode = mode;
        entry
    }

    #[test]
    fn rows_use_game_tablist_order_and_display_names() {
        let mut tabs = TabList::new();
        let mut bob = entry(2, "Bob", 30, GameMode::Spectator);
        bob.display_name = Some(Text::literal("Bob AFK"));
        tabs.insert(bob);
        tabs.insert(entry(1, "Alice", 12, GameMode::Survival));

        assert_eq!(
            player_rows(&tabs),
            vec!["Alice  12ms".to_string(), "Bob AFK  30ms".to_string()]
        );
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn tab_overlay_reaches_pixels_inside_panel_rect() {
        use crate::hud::{DebugStats, HudFrame, HudRenderer};
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in but no adapter is available; do not treat this as a pass",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (640u32, 480u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        let mut tabs = TabList::new();
        tabs.insert(entry(1, "Alice", 12, GameMode::Survival));
        tabs.insert(entry(2, "Bob", 30, GameMode::Survival));
        let rows = player_rows(&tabs);

        let mut render = |players: Option<&[String]>| -> usize {
            let frame = target.acquire().expect("headless acquire");
            clear(device, queue, frame.view());
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                players,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            count_changed(&pixels, w, h, w / 4, h / 4, w / 2, h / 2)
        };

        let blank = render(None);
        let populated = render(Some(&rows));
        assert_eq!(blank, 0, "empty tab overlay region must be untouched");
        assert!(
            populated > 1_000,
            "tab rows must paint panel/text pixels in the overlay rect, got {populated}"
        );
    }

    fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tablist-clear"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tablist-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 128.0 / 255.0,
                            g: 128.0 / 255.0,
                            b: 128.0 / 255.0,
                            a: 1.0,
                        }),
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

    fn count_changed(
        pixels: &[u8],
        width: u32,
        _height: u32,
        x0: u32,
        y0: u32,
        rw: u32,
        rh: u32,
    ) -> usize {
        let mut changed = 0;
        for y in y0..(y0 + rh) {
            for x in x0..(x0 + rw) {
                let i = ((y * width + x) * 4) as usize;
                let d = (i32::from(pixels[i]) - 128).abs()
                    + (i32::from(pixels[i + 1]) - 128).abs()
                    + (i32::from(pixels[i + 2]) - 128).abs();
                if d > 25 {
                    changed += 1;
                }
            }
        }
        changed
    }
}
