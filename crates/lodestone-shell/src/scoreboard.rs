//! Scoreboard display projection.
//!
//! `lodestone-game` owns the scoreboard fold and team/name semantics. The shell
//! only projects that folded state into the existing HUD sidebar rows.

use lodestone_game::scoreboard::{NumberFormat, Scoreboard};
use lodestone_model::Text;

use crate::overlay::{Sidebar, SidebarLine};

const MAX_SIDEBAR_LINES: usize = 15;

/// Resolve a component's `translate` nodes with `translate`, then flatten to a
/// plain string — the shape the HUD sidebar draws. Keeps this module free of the
/// language table itself: the caller supplies the translator closure.
fn plain(text: &Text, translate: &dyn Fn(&str) -> Option<String>) -> String {
    lodestone_game::text::resolve(text, translate).to_plain_string()
}

/// Builds the right-edge sidebar view from the folded game scoreboard, resolving
/// server-authored `translate` components (objective title, per-holder display
/// names, fixed number formats) against `translate`.
#[must_use]
pub fn sidebar_from(
    scoreboard: &Scoreboard,
    translate: &dyn Fn(&str) -> Option<String>,
) -> Option<Sidebar> {
    let objective_name = scoreboard.displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar)?;
    let objective = scoreboard.objective(objective_name)?;
    let lines = scoreboard
        .sorted_scores(objective_name)
        .into_iter()
        .take(MAX_SIDEBAR_LINES)
        .map(|(holder, entry)| {
            let number_format = if matches!(entry.number_format, NumberFormat::Default) {
                &objective.number_format
            } else {
                &entry.number_format
            };
            SidebarLine {
                label: plain(
                    &entry
                        .display_name
                        .as_ref()
                        .map_or_else(|| scoreboard.display_name_of(holder), Clone::clone),
                    translate,
                ),
                score: score_text(entry.value, number_format, translate),
            }
        })
        .collect();
    Some(Sidebar {
        title: plain(&objective.display_name, translate),
        lines,
    })
}

fn score_text(
    value: i32,
    format: &NumberFormat,
    translate: &dyn Fn(&str) -> Option<String>,
) -> String {
    match format {
        NumberFormat::Blank => String::new(),
        NumberFormat::Fixed(text) => plain(text, translate),
        NumberFormat::Default | NumberFormat::Styled(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::scoreboard::{DisplaySlot, Objective, ScoreEntry};
    use lodestone_model::Text;

    /// A translator that resolves nothing (the demo-palette case).
    fn no_tr(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn sidebar_uses_game_scoreboard_scores_and_display_names() {
        let mut scoreboard = Scoreboard::new();
        scoreboard.add_objective(Objective::new("kills", "", Text::literal("Kills")));
        scoreboard.set_display(DisplaySlot::Sidebar, Some("kills"));
        scoreboard.set_score_entry(
            "kills",
            "Alice",
            ScoreEntry {
                value: 7,
                display_name: Some(Text::literal("Alice the Brave")),
                number_format: NumberFormat::Default,
            },
        );
        scoreboard.set_score("kills", "Bob", 3);

        let side = sidebar_from(&scoreboard, &no_tr).expect("sidebar visible");
        assert_eq!(side.title, "Kills");
        let rows: Vec<(&str, &str)> = side
            .lines
            .iter()
            .map(|line| (line.label.as_str(), line.score.as_str()))
            .collect();
        assert_eq!(rows, vec![("Alice the Brave", "7"), ("Bob", "3")]);
    }

    #[test]
    fn sidebar_honours_blank_and_fixed_number_formats() {
        let mut scoreboard = Scoreboard::new();
        let mut objective = Objective::new("obj", "", Text::literal("Obj"));
        objective.number_format = NumberFormat::Blank;
        scoreboard.add_objective(objective);
        scoreboard.set_display(DisplaySlot::Sidebar, Some("obj"));
        scoreboard.set_score("obj", "Hidden", 9);
        scoreboard.set_score_entry(
            "obj",
            "Fixed",
            ScoreEntry {
                value: 8,
                display_name: None,
                number_format: NumberFormat::Fixed(Box::new(Text::literal("ok"))),
            },
        );

        let side = sidebar_from(&scoreboard, &no_tr).expect("sidebar visible");
        let rows: Vec<(&str, &str)> = side
            .lines
            .iter()
            .map(|line| (line.label.as_str(), line.score.as_str()))
            .collect();
        assert_eq!(rows, vec![("Hidden", ""), ("Fixed", "ok")]);
    }

    #[test]
    fn sidebar_resolves_translate_components_through_the_translator() {
        let mut scoreboard = Scoreboard::new();
        // A server can send the objective title and a holder display name as
        // `translate` components; the sidebar must render the resolved words.
        scoreboard.add_objective(Objective::new(
            "obj",
            "",
            Text::translate("gui.stats", vec![]),
        ));
        scoreboard.set_display(DisplaySlot::Sidebar, Some("obj"));
        scoreboard.set_score_entry(
            "obj",
            "spider",
            ScoreEntry {
                value: 1,
                display_name: Some(Text::translate("entity.minecraft.spider", vec![])),
                number_format: NumberFormat::Default,
            },
        );

        let tr = |key: &str| match key {
            "gui.stats" => Some("Statistics".to_string()),
            "entity.minecraft.spider" => Some("Spider".to_string()),
            _ => None,
        };
        let side = sidebar_from(&scoreboard, &tr).expect("sidebar visible");
        assert_eq!(side.title, "Statistics");
        assert_eq!(side.lines[0].label, "Spider");

        // Negative control: without the table, the raw key leaks through — the
        // exact defect this wiring fixes.
        let raw = sidebar_from(&scoreboard, &no_tr).expect("sidebar visible");
        assert_eq!(raw.lines[0].label, "entity.minecraft.spider");
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn sidebar_reaches_pixels_inside_right_edge_rect() {
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

        let mut scoreboard = Scoreboard::new();
        scoreboard.add_objective(Objective::new("kills", "", Text::literal("Kills")));
        scoreboard.set_display(DisplaySlot::Sidebar, Some("kills"));
        scoreboard.set_score("kills", "Alice", 7);
        scoreboard.set_score("kills", "Bob", 3);
        let sidebar = sidebar_from(&scoreboard, &no_tr).expect("sidebar visible");

        let mut render = |side: Option<&Sidebar>| -> usize {
            let frame = target.acquire().expect("headless acquire");
            clear(device, queue, frame.view());
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                sidebar: side,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            count_changed(&pixels, w, h, w * 2 / 3, h / 4, w / 3, h / 2)
        };

        let blank = render(None);
        let populated = render(Some(&sidebar));
        assert_eq!(blank, 0, "empty sidebar region must be untouched");
        assert!(
            populated > 300,
            "sidebar title/rows must paint pixels in the right-edge rect, got {populated}"
        );
    }

    fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("scoreboard-clear"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scoreboard-clear-pass"),
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
