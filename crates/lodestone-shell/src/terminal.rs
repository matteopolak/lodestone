//! Native terminal presentation surfaces.
//!
//! `stdio` is deliberately GPU-free and emits only player-visible text. The
//! `terminal` surface drives the normal [`crate::sim::Sim`], renders its camera
//! through the existing offscreen wgpu target, and hands the RGBA readback to
//! `ratatui-image`'s true-colour Unicode half-block protocol. Ratatui owns the
//! surrounding chat, status, and input panes. Neither path creates a window.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use image::{DynamicImage, RgbaImage};
use lodestone_controller::Action;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget, TargetError};
use ratatui::crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui_image::Image;
use ratatui_image::protocol::{Protocol, halfblocks::Halfblocks};

use crate::chat::compose_chat_action;
use crate::config::Config;
use crate::gpu::RenderState;
use crate::net::{NetClient, NetUpdate};
use crate::platform::Instant;
use crate::sim::Sim;

enum Input {
    Line(String),
    Closed,
}

fn input_lines() -> Receiver<Input> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Input::Closed);
                    break;
                }
                Ok(_) => {
                    if tx.send(Input::Line(line.trim_end().to_owned())).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    eprintln!("terminal input failed: {error}");
                    let _ = tx.send(Input::Closed);
                    break;
                }
            }
        }
    });
    rx
}

/// Run the newline-delimited, GPU-free chat surface.
pub(crate) fn run_stdio(
    _owned: lodestone_auth::Entitlement,
    config: Config,
) -> anyhow::Result<()> {
    println!(
        "connecting to {}:{} (protocol {}) — type chat or /commands; #quit exits",
        config.host, config.port, config.protocol
    );
    let net = NetClient::connect(
        config.host.clone(),
        Some(config.port),
        config.protocol,
        None,
    );
    let input = input_lines();
    let mut running = true;

    while running {
        while let Ok(event) = input.try_recv() {
            match event {
                Input::Line(line) if line.trim() == "#quit" => running = false,
                Input::Line(line) => {
                    if let Some(action) = compose_chat_action(&line) {
                        net.send_action(action);
                    }
                }
                Input::Closed => running = false,
            }
        }
        for update in net.poll() {
            match update {
                NetUpdate::ConnectPhase(phase) => println!("[connection] {phase:?}"),
                NetUpdate::LoggedIn { .. } => println!("[connection] joined"),
                NetUpdate::Chat { text, .. } | NetUpdate::ActionBar(text) => {
                    println!("{}", text.to_plain_string());
                }
                NetUpdate::Disconnected(reason) => {
                    println!("[connection] {}", reason.to_plain_string());
                    running = false;
                }
                NetUpdate::Error(error) => {
                    eprintln!("[connection] {error}");
                    running = false;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Game,
    Chat,
}

#[derive(Clone, Copy)]
struct SurfaceAreas {
    chat: Rect,
    game: Rect,
    input: Rect,
}

fn surface_areas(area: Rect) -> SurfaceAreas {
    let [body, input] = Layout::vertical([Constraint::Min(5), Constraint::Length(3)]).areas(area);
    let [chat, game] =
        Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)]).areas(body);
    SurfaceAreas { chat, game, input }
}

fn inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

fn current_area() -> io::Result<Rect> {
    let (width, height) = ratatui::crossterm::terminal::size()?;
    Ok(Rect::new(0, 0, width, height))
}

fn render_dimensions(area: Rect) -> (u32, u32) {
    let area = inner(surface_areas(area).game);
    (u32::from(area.width.max(1)), u32::from(area.height.max(1)) * 2)
}

/// Run the live game in a Ratatui layout with a coloured Unicode image pane.
pub(crate) fn run_terminal(
    _owned: lodestone_auth::Entitlement,
    config: Config,
) -> anyhow::Result<()> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        anyhow::bail!(
            "--surface terminal requires stdin and stdout terminals; use --surface stdio for pipes"
        );
    }

    let initial_area = current_area()?;
    let (mut pixel_width, mut pixel_height) = render_dimensions(initial_area);
    let ctx = GpuContext::new_headless_blocking()
        .map_err(|error| anyhow::anyhow!("terminal GPU bring-up failed: {error}"))?;
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, pixel_width, pixel_height, format);
    let mut sim = Sim::new(config.clone());
    sim.connect(config.host.clone(), Some(config.port), config.protocol);
    let mut render = RenderState::new(
        device,
        queue,
        format,
        pixel_width,
        pixel_height,
        sim.vanilla_atlas(),
    );
    render.set_fog(
        crate::sim::fog_for_render_distance(config.render_distance),
        config.render_distance,
    );

    let mut terminal = ratatui::try_init()?;
    if let Err(error) = execute!(
        io::stdout(),
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    ) {
        ratatui::restore();
        return Err(error.into());
    }
    let _session = TerminalSession;

    let mut focus = Focus::Game;
    let mut chat_input = String::new();
    let mut last_mouse = None;
    let mut held = HashMap::new();
    let mut last_frame = Instant::now();
    let mut running = true;

    while running {
        let area = current_area()?;
        let areas = surface_areas(area);
        while event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(key) => {
                    if !handle_key(key, &mut focus, &mut chat_input, &mut held, &mut sim) {
                        running = false;
                        break;
                    }
                }
                Event::Mouse(mouse) if focus == Focus::Game => {
                    if matches!(mouse.kind, MouseEventKind::Moved) {
                        let game = inner(areas.game);
                        if game.contains((mouse.column, mouse.row).into()) {
                            if let Some((old_x, old_y)) = last_mouse {
                                sim.input_mut(|input| {
                                    input.add_mouse(
                                        f32::from(mouse.column) - f32::from(old_x),
                                        f32::from(mouse.row) - f32::from(old_y),
                                    );
                                });
                            }
                            last_mouse = Some((mouse.column, mouse.row));
                        } else {
                            last_mouse = None;
                        }
                    }
                }
                Event::Resize(_, _) => last_mouse = None,
                _ => {}
            }
        }
        if focus == Focus::Chat {
            last_mouse = None;
        }
        if !running {
            break;
        }
        expire_unreleased_keys(&mut held, &mut sim);

        let dimensions = render_dimensions(area);
        if dimensions != (pixel_width, pixel_height) {
            (pixel_width, pixel_height) = dimensions;
            target = HeadlessTarget::new(device, pixel_width, pixel_height, format);
            render.resize(device, pixel_width, pixel_height);
        }

        let now = Instant::now();
        let dt = now.duration_since(last_frame).as_secs_f64().min(0.25);
        last_frame = now;
        sim.step(dt);

        for key in sim.drain_removals() {
            render.remove_section(&key);
        }
        for meshed in sim.drain_meshes() {
            render.upload_section(device, queue, meshed.key, &meshed.mesh);
        }

        let camera = sim.camera(pixel_width as f32 / pixel_height as f32);
        let frame = target
            .acquire()
            .map_err(|error: TargetError| {
                anyhow::anyhow!("terminal frame acquire failed: {error}")
            })?;
        let entity_draws = sim.entity_draws();
        let _ = sim.extract_particles(&camera);
        render.prepare_particles(device, queue, &sim.particle_instances(), &camera);
        render.update_animation(queue, sim.tick_count());
        let _ = render.render(
            device,
            queue,
            frame.view(),
            &camera,
            None,
            &entity_draws,
        );
        let protocol = halfblock_protocol(
            target.read_texels(device, queue),
            pixel_width,
            pixel_height,
        )?;

        let chat = sim
            .recent_chat_spans(usize::from(areas.chat.height.saturating_sub(2)))
            .into_iter()
            .map(|(spans, _)| {
                spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let position = sim.player().position;
        let game_title = format!(
            " Game · {:?} · {:.1} {:.1} {:.1} ",
            sim.session_phase(), position.x, position.y, position.z
        );
        terminal.draw(|frame| {
            let areas = surface_areas(frame.area());
            frame.render_widget(Block::bordered().title(" Chat "), areas.chat);
            frame.render_widget(
                Paragraph::new(chat.as_str()).wrap(Wrap { trim: false }),
                inner(areas.chat),
            );
            frame.render_widget(Block::bordered().title(game_title.as_str()), areas.game);
            frame.render_widget(Image::new(&protocol), inner(areas.game));
            let input_title = match focus {
                Focus::Game => " Input · Enter or / to chat · Ctrl-C to quit ",
                Focus::Chat => " Chat · Enter to send · Esc to cancel ",
            };
            let style = if focus == Focus::Chat {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            frame.render_widget(
                Paragraph::new(format!("> {chat_input}"))
                    .block(Block::bordered().title(input_title))
                    .style(style),
                areas.input,
            );
        })?;

        std::thread::sleep(Duration::from_millis(80));
    }
    sim.input_mut(|input| input.release_all());
    Ok(())
}

fn handle_key(
    key: KeyEvent,
    focus: &mut Focus,
    chat_input: &mut String,
    held: &mut HashMap<Action, Instant>,
    sim: &mut Sim,
) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return false;
    }

    match *focus {
        Focus::Chat if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            match key.code {
                KeyCode::Esc => {
                    chat_input.clear();
                    *focus = Focus::Game;
                }
                KeyCode::Enter => {
                    let line = std::mem::take(chat_input);
                    if line.trim() == "#quit" {
                        return false;
                    }
                    if !line.is_empty() {
                        let _ = sim.send_chat(&line);
                    }
                    *focus = Focus::Game;
                }
                KeyCode::Backspace => {
                    chat_input.pop();
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    chat_input.push(ch);
                }
                _ => {}
            }
        }
        Focus::Game => {
            if matches!(key.kind, KeyEventKind::Press) {
                match key.code {
                    KeyCode::Enter => {
                        sim.input_mut(|input| input.release_all());
                        held.clear();
                        *focus = Focus::Chat;
                    }
                    KeyCode::Char('/') => {
                        sim.input_mut(|input| input.release_all());
                        held.clear();
                        chat_input.push('/');
                        *focus = Focus::Chat;
                    }
                    _ => {}
                }
            }
            if let Some(action) = movement_action(key.code) {
                let is_held = !matches!(key.kind, KeyEventKind::Release);
                sim.input_mut(|input| input.set(action, is_held));
                if is_held {
                    held.insert(action, Instant::now());
                } else {
                    held.remove(&action);
                }
            }
        }
        Focus::Chat => {}
    }
    true
}

fn movement_action(key: KeyCode) -> Option<Action> {
    Some(match key {
        KeyCode::Char('w' | 'W') => Action::Forward,
        KeyCode::Char('s' | 'S') => Action::Back,
        KeyCode::Char('a' | 'A') => Action::Left,
        KeyCode::Char('d' | 'D') => Action::Right,
        KeyCode::Char(' ') => Action::Jump,
        _ => return None,
    })
}

fn expire_unreleased_keys(held: &mut HashMap<Action, Instant>, sim: &mut Sim) {
    const RELEASE_TIMEOUT: Duration = Duration::from_millis(350);
    let expired = held
        .iter()
        .filter_map(|(action, pressed)| (pressed.elapsed() >= RELEASE_TIMEOUT).then_some(*action))
        .collect::<Vec<_>>();
    for action in expired {
        held.remove(&action);
        sim.input_mut(|input| input.set(action, false));
    }
}

fn halfblock_protocol(
    mut rgba: Vec<u8>,
    width: u32,
    height: u32,
) -> anyhow::Result<Protocol> {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = linear_to_srgb_byte(pixel[0]);
        pixel[1] = linear_to_srgb_byte(pixel[1]);
        pixel[2] = linear_to_srgb_byte(pixel[2]);
    }
    let image = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| anyhow::anyhow!("terminal renderer returned an invalid RGBA frame"))?;
    let cells = Size::new(width as u16, height.div_ceil(2) as u16);
    Ok(Protocol::Halfblocks(Halfblocks::new(
        DynamicImage::ImageRgba8(image),
        cells,
    )?))
}

fn linear_to_srgb_byte(value: u8) -> u8 {
    let linear = f32::from(value) / 255.0;
    let srgb = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}

struct TerminalSession;

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture
        );
        ratatui::restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_reserves_chat_game_and_input_panes() {
        let areas = surface_areas(Rect::new(0, 0, 100, 30));
        assert_eq!(areas.input.height, 3);
        assert_eq!(areas.chat.width, 32);
        assert_eq!(areas.game.width, 68);
        assert_eq!(render_dimensions(Rect::new(0, 0, 100, 30)), (66, 50));
    }

    #[test]
    fn linear_target_colours_are_encoded_for_a_terminal() {
        assert_eq!(linear_to_srgb_byte(0), 0);
        assert_eq!(linear_to_srgb_byte(128), 188);
        assert_eq!(linear_to_srgb_byte(255), 255);
    }

    #[test]
    fn terminal_movement_keys_share_the_controller_actions() {
        assert_eq!(movement_action(KeyCode::Char('w')), Some(Action::Forward));
        assert_eq!(movement_action(KeyCode::Char('D')), Some(Action::Right));
        assert_eq!(movement_action(KeyCode::Char('x')), None);
    }

    #[test]
    fn rgba_frame_uses_the_library_halfblock_protocol() {
        let protocol = halfblock_protocol(vec![0, 0, 0, 255, 255, 255, 255, 255], 1, 2)
            .expect("valid RGBA frame");
        assert_eq!(protocol.size(), Size::new(1, 1));
    }
}
