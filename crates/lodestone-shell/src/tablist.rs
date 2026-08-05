//! Tab-list display projection.
//!
//! The authoritative fold lives in [`lodestone_game::tablist::TabList`]. This
//! module only lowers that state into the flat text rows the shell HUD already
//! draws while Tab is held.

use lodestone_game::tablist::TabList;

/// Formats the listed players in vanilla display order as HUD rows, resolving
/// any `translate` components in a player's display name against `translate`.
#[must_use]
pub fn player_rows(tab_list: &TabList, translate: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    tab_list
        .ordered()
        .into_iter()
        .map(|entry| {
            let name =
                lodestone_game::text::resolve(&entry.effective_name(), translate).to_plain_string();
            if entry.latency >= 0 {
                format!("{name}  {}ms", entry.latency)
            } else {
                format!("{name}  --")
            }
        })
        .collect()
}

/// Lowers a tab-list header or footer into the centred lines the HUD draws
/// above and below the player rows.
///
/// A `Text` is a *tree*, and a server writes a multi-line banner as literal
/// `\n` inside it — so resolving to a plain string and splitting is the whole
/// job. An absent, empty or whitespace-only banner yields **no lines**, which
/// is what makes `Option`-ing the result at the call site unnecessary: vanilla
/// draws nothing for an empty header rather than an empty gap
/// (`PlayerTabOverlay.render`, which only measures a non-null header).
///
/// Trailing empties are dropped but interior blank lines are kept: a server
/// separating two banner halves with a blank line means it.
#[must_use]
pub fn banner_lines(
    banner: Option<&lodestone_model::Text>,
    translate: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let Some(banner) = banner else {
        return Vec::new();
    };
    let plain = lodestone_game::text::resolve(banner, translate).to_plain_string();
    if plain.trim().is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = plain.split('\n').map(str::to_string).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
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

    /// A translator that resolves nothing (the demo-palette case).
    fn no_tr(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn rows_use_game_tablist_order_and_display_names() {
        let mut tabs = TabList::new();
        let mut bob = entry(2, "Bob", 30, GameMode::Spectator);
        bob.display_name = Some(Text::literal("Bob AFK"));
        tabs.insert(bob);
        tabs.insert(entry(1, "Alice", 12, GameMode::Survival));

        assert_eq!(
            player_rows(&tabs, &no_tr),
            vec!["Alice  12ms".to_string(), "Bob AFK  30ms".to_string()]
        );
    }

    #[test]
    fn rows_resolve_translate_display_names_through_the_translator() {
        let mut tabs = TabList::new();
        let mut e = entry(1, "Steve", 20, GameMode::Survival);
        e.display_name = Some(Text::translate("entity.minecraft.spider", vec![]));
        tabs.insert(e);

        let tr = |key: &str| (key == "entity.minecraft.spider").then(|| "Spider".to_string());
        assert_eq!(player_rows(&tabs, &tr), vec!["Spider  20ms".to_string()]);
        // Negative control: no table leaks the raw key.
        assert_eq!(
            player_rows(&tabs, &no_tr),
            vec!["entity.minecraft.spider  20ms".to_string()]
        );
    }

    #[test]
    fn a_banner_splits_on_newlines_and_an_absent_one_yields_no_lines() {
        assert_eq!(banner_lines(None, &no_tr), Vec::<String>::new());
        assert_eq!(
            banner_lines(Some(&Text::literal("Welcome")), &no_tr),
            vec!["Welcome".to_string()]
        );
        assert_eq!(
            banner_lines(Some(&Text::literal("Top\nMiddle\nBottom")), &no_tr),
            vec![
                "Top".to_string(),
                "Middle".to_string(),
                "Bottom".to_string()
            ]
        );
        // An empty banner draws nothing rather than an empty gap — the reason
        // the HUD field is a possibly-empty slice and not an `Option`.
        assert_eq!(banner_lines(Some(&Text::literal("")), &no_tr), Vec::<String>::new());
        assert_eq!(
            banner_lines(Some(&Text::literal("   \n  ")), &no_tr),
            Vec::<String>::new()
        );
        // A trailing blank line is dropped; an *interior* one is kept, because a
        // server separating two banner halves with a gap means it.
        assert_eq!(
            banner_lines(Some(&Text::literal("A\n\nB\n\n")), &no_tr),
            vec!["A".to_string(), String::new(), "B".to_string()]
        );
    }

    #[test]
    fn a_banner_resolves_translate_components_through_the_translator() {
        let banner = Text::translate("multiplayer.title", vec![]);
        let tr = |key: &str| (key == "multiplayer.title").then(|| "Servers".to_string());
        assert_eq!(
            banner_lines(Some(&banner), &tr),
            vec!["Servers".to_string()]
        );
        // Negative control: with no table the raw key leaks, so the assertion
        // above is really measuring the translator and not a literal.
        assert_eq!(
            banner_lines(Some(&banner), &no_tr),
            vec!["multiplayer.title".to_string()]
        );
    }

    /// **Measures by location, not by frame average.** The header and footer
    /// are *centred* while the caption and player rows are *left-aligned*, so
    /// the discriminating question is not "did more pixels light up" (which a
    /// header drawn in the wrong place would also satisfy) but "where is the
    /// horizontal centre of mass of the text on this scanline band".
    ///
    /// Every band and the panel rect come from [`crate::hud::TabPanel`] — the
    /// same value the draw lays out from — so this cannot drift into passing
    /// against a panel that has moved.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn the_header_draws_centred_above_the_rows_and_the_footer_centred_below() {
        use crate::hud::{DebugStats, HudFrame, HudRenderer, TabPanel};
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
        let rows = player_rows(&tabs, &no_tr);
        let header = vec!["HEADER".to_string()];
        let footer = vec!["FOOTER".to_string()];

        let mut render = |hdr: &[String], ftr: &[String]| -> Vec<u8> {
            let frame = target.acquire().expect("headless acquire");
            clear(device, queue, frame.view());
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                players: Some(&rows),
                tab_header: hdr,
                tab_footer: ftr,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            target.read_texels(device, queue)
        };

        let with_banner = render(&header, &footer);
        let without = render(&[], &[]);

        // The canvas the HUD actually lays out into is the *logical* one, not
        // the framebuffer — reading `b.w`/`b.h`'s source rather than assuming
        // 640x480 is the difference between a gate and a coincidence.
        let (cw, ch) =
            crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let sx = f64::from(w) / f64::from(cw as u32).max(1.0);
        let sy = f64::from(h) / f64::from(ch as u32).max(1.0);
        let banner_w = 0.0f32; // "HEADER"/"FOOTER" are far narrower than cw/2.
        let panel = TabPanel::new(cw, ch, header.len(), rows.len(), footer.len(), banner_w);
        let bare = TabPanel::new(cw, ch, 0, rows.len(), 0, 0.0);

        // Centre of mass of lit pixels on one logical scanline band, in
        // framebuffer x. `None` when the band is blank.
        let band_centre = |px: &[u8], y0: f32, y1: f32| -> Option<(f64, u32, u32)> {
            let (mut sum, mut n) = (0f64, 0u64);
            let (mut lo, mut hi) = (u32::MAX, 0u32);
            let y_start = (f64::from(y0) * sy).round().max(0.0) as u32;
            let y_end = ((f64::from(y1) * sy).round() as u32).min(h);
            // Scan only *inside* the panel. Outside it the clear colour is grey
            // 128, and a threshold that had to exclude that as well would also
            // have to exclude the caption, whose blue channel is 153 — the
            // first version of this gate did exactly that and read the caption
            // band as blank. Against the panel's own ~38-per-channel backdrop
            // the separation is unambiguous.
            let x_start = (f64::from(panel.x) * sx).round().max(0.0) as u32;
            let x_end = ((f64::from(panel.x + panel.w) * sx).round() as u32).min(w);
            for y in y_start..y_end {
                for x in x_start..x_end {
                    let i = ((y * w + x) * 4) as usize;
                    if px[i] > 120 && px[i + 1] > 120 && px[i + 2] > 120 {
                        sum += f64::from(x);
                        n += 1;
                        lo = lo.min(x);
                        hi = hi.max(x);
                    }
                }
            }
            (n > 0).then(|| (sum / n as f64, lo, hi))
        };

        let panel_centre_fb = f64::from(panel.centre_x()) * sx;
        let left_fb = f64::from(panel.left_x()) * sx;

        let hdr = band_centre(&with_banner, panel.header_y(0), panel.header_y(1))
            .unwrap_or_else(|| {
                panic!(
                    "no header text lit in band y=[{}, {}] (logical); panel={panel:?} — \
                     this is the island: the field is folded and nothing draws it",
                    panel.header_y(0),
                    panel.header_y(1)
                )
            });
        let ftr = band_centre(
            &with_banner,
            panel.footer_y(0),
            panel.footer_y(0) + panel.line_h,
        )
        .unwrap_or_else(|| {
            panic!(
                "no footer text lit in band y=[{}, {}] (logical); panel={panel:?}",
                panel.footer_y(0),
                panel.footer_y(0) + panel.line_h
            )
        });
        // The caption band, for the left-aligned rival hypothesis.
        let cap = band_centre(&with_banner, panel.header_y(1), panel.header_y(2))
            .expect("the PLAYERS caption must be lit");

        eprintln!(
            "header centre={:.1} bbox=({}, {})\nfooter centre={:.1} bbox=({}, {})\n\
             caption centre={:.1} bbox=({}, {})\npanel centre={panel_centre_fb:.1} left={left_fb:.1}",
            hdr.0, hdr.1, hdr.2, ftr.0, ftr.1, ftr.2, cap.0, cap.1, cap.2
        );

        // **Two hypotheses, no magic tolerance.** An earlier version of this
        // asserted only "the centre of mass is within one line height of the
        // panel centre", and the control below failed it: the *left-aligned*
        // caption cleared that too (35.5 against a 36.0 tolerance), because
        // "PLAYERS (2)" is wide enough that its own centre drifts most of the
        // way to the panel's. A one-sided distance cannot tell centred from
        // merely-long. So compute both rivals and require the measurement to
        // land decisively on one:
        //
        //   d_centred = |bbox centre - panel centre|   (the centred hypothesis)
        //   d_left    = |bbox left   - panel inset |   (the left-aligned rival)
        //
        // Centred text has d_centred << d_left; left-aligned text the reverse.
        let verdict = |(_, lo, hi): (f64, u32, u32)| -> (f64, f64) {
            let centre = (f64::from(lo) + f64::from(hi)) * 0.5;
            ((centre - panel_centre_fb).abs(), (f64::from(lo) - left_fb).abs())
        };
        let (h_centred, h_left) = verdict(hdr);
        let (f_centred, f_left) = verdict(ftr);
        let (c_centred, c_left) = verdict(cap);
        eprintln!(
            "d_centred/d_left — header {h_centred:.1}/{h_left:.1}, \
             footer {f_centred:.1}/{f_left:.1}, caption {c_centred:.1}/{c_left:.1}"
        );

        assert!(
            h_centred * 4.0 < h_left,
            "the header must be CENTRED, not left-aligned: d_centred={h_centred:.1} \
             is not decisively below d_left={h_left:.1}; bbox=({}, {})",
            hdr.1,
            hdr.2
        );
        assert!(
            f_centred * 4.0 < f_left,
            "the footer must be CENTRED: d_centred={f_centred:.1} vs d_left={f_left:.1}; \
             bbox=({}, {})",
            ftr.1,
            ftr.2
        );
        // The control, measured on the same frame rather than asserted from a
        // constant: the caption really is drawn left-aligned, so it must land
        // on the *other* hypothesis. If it did not, the predicate above would
        // be satisfied by any text at all.
        assert!(
            c_left * 4.0 < c_centred,
            "the left-aligned caption must land on the left hypothesis \
             (d_left={c_left:.1}, d_centred={c_centred:.1}), or this gate does not \
             discriminate alignment"
        );

        // And the header really is *above* the rows and the footer *below*:
        // the bare panel is shorter, so its own top band holds the caption, and
        // the banner frame's top band holds text the bare frame does not.
        assert!(
            panel.header_y(0) < bare.header_y(0),
            "adding a header must grow the panel upward, not leave it in place"
        );
        let bare_top = band_centre(&without, bare.header_y(0), bare.header_y(1))
            .expect("the bare frame's top band is the caption");
        let (bt_centred, bt_left) = verdict(bare_top);
        assert!(
            bt_left * 4.0 < bt_centred,
            "with no header the panel's top band is the left-aligned caption \
             (d_left={bt_left:.1}, d_centred={bt_centred:.1}) — if this read as centred, \
             the gate above would pass with the feature deleted"
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
        let rows = player_rows(&tabs, &no_tr);

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
