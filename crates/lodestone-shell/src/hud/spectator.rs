use super::Builder;
use crate::menu::spectator_menu::SpectatorHotbarView;

pub(super) fn draw(b: &mut Builder<'_>, view: &SpectatorHotbarView) {
    let x = (b.w * 0.5).floor() - 91.0;
    let y = (b.h - 22.0 * view.alpha).floor();
    let white = [1.0, 1.0, 1.0, view.alpha];
    if b.gui.is_some_and(|gui| gui.contains("hud/hotbar")) {
        b.sprite("hud/hotbar", x, y, 182.0, 22.0, white);
        if let Some(selected) = view.selected {
            let sx = x - 1.0 + selected as f32 * 20.0;
            if b.gui.is_some_and(|gui| gui.contains("hud/hotbar_selection")) {
                b.sprite("hud/hotbar_selection", sx, y - 1.0, 24.0, 23.0, white);
            } else {
                b.rect_px(sx, y - 1.0, 24.0, 23.0, white);
                b.rect_px(sx + 2.0, y + 1.0, 20.0, 19.0, [0.1, 0.1, 0.1, view.alpha]);
            }
        }
    } else {
        b.rect_px(x, y, 182.0, 22.0, [0.12, 0.12, 0.12, view.alpha * 0.8]);
        for slot in 0..9 {
            let sx = x + 1.0 + slot as f32 * 20.0;
            b.rect_px(sx, y + 1.0, 20.0, 20.0, [0.35, 0.35, 0.35, view.alpha]);
            let inset = if view.selected == Some(slot) { 2.0 } else { 1.0 };
            b.rect_px(sx + inset, y + 1.0 + inset, 20.0 - 2.0 * inset, 20.0 - 2.0 * inset, [0.1, 0.1, 0.1, view.alpha]);
        }
    }
    for (slot, item) in view.slots.iter().enumerate() {
        let Some(item) = item else { continue };
        let sx = x + 3.0 + slot as f32 * 20.0;
        let brightness = if item.enabled { 1.0 } else { 0.25 };
        let colour = [brightness, brightness, brightness, view.alpha];
        if b.gui.is_some_and(|gui| gui.contains(item.sprite)) {
            b.sprite(item.sprite, sx, y + 3.0, 16.0, 16.0, colour);
        } else {
            let label = match item.sprite {
                "spectator/teleport_to_team" => "T",
                "spectator/close" => "X",
                "spectator/scroll_left" => "<",
                "spectator/scroll_right" => ">",
                _ => "P",
            };
            b.text(label, sx + 4.0, y + 4.0, 1.0, colour);
        }
        if item.enabled {
            let key = (slot + 1).to_string();
            let width = b.text_width(&key, 1.0);
            b.text(&key, sx + 17.0 - width, y + 12.0, 1.0, white);
        }
    }
    let width = b.text_width(&view.prompt, 1.0);
    b.text(&view.prompt, (b.w - width) * 0.5, b.h - 35.0, 1.0, white);
}

#[cfg(test)]
mod tests {
    use crate::hud::{DebugStats, HudFrame, HudGeometry};
    use crate::menu::spectator_menu::SpectatorMenuState;
    use lodestone_assets::{MemorySource, ResourceManager, ResourceSource};
    use lodestone_render::GuiAtlas;

    fn atlas(ids: &[&str]) -> GuiAtlas {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(&[255; 4]).unwrap();
        }
        let mut source = MemorySource::new("spectator-test");
        for id in ids {
            source.insert(format!("assets/minecraft/textures/gui/sprites/{id}.png"), png.clone());
        }
        let manager = ResourceManager::new(vec![Box::new(source) as Box<dyn ResourceSource>]);
        GuiAtlas::build(&manager).unwrap()
    }

    #[test]
    fn spectator_selector_draws_real_sprite_geometry_and_missing_sprite_fallbacks() {
        let stats = DebugStats::default();
        let mut selector = SpectatorMenuState::default();
        selector.select(None, 0.0);
        let view = selector.view(0.0).unwrap();
        let frame = HudFrame {
            show_debug: false,
            crosshair: false,
            spectator_hotbar: Some(&view),
            ..HudFrame::new(&stats)
        };
        let complete = atlas(&[
            "hud/hotbar", "spectator/teleport_to_player", "spectator/teleport_to_team",
            "spectator/scroll_right", "spectator/close",
        ]);
        let control = HudGeometry::build_with_gui(&frame, 640, 480, &complete);
        assert_eq!(control.sprite_vertex_count(), 5 * 6, "bar and four root icons");
        let incomplete = atlas(&["unrelated"]);
        let fallback = HudGeometry::build_with_gui(&frame, 640, 480, &incomplete);
        assert_eq!(fallback.sprite_vertex_count(), 0);
        assert!(fallback.vertex_count() > control.vertex_count(),
            "an attached atlas missing selector sprites must still draw its bar and icons");
        assert!(fallback.verts.chunks_exact(6).all(|v| v[1] < 0.0));
    }
}
