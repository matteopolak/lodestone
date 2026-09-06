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
use lodestone_assets::ResourceLocation;
use lodestone_controller::Action;
use lodestone_game::click::{Click, PlayerCtx};
use lodestone_game::menu::Menu;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget, TargetError};
use ratatui::crossterm::{
    event::{
        self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui_image::Image;
use ratatui_image::protocol::{Protocol, halfblocks::Halfblocks};

use crate::chat::compose_chat_action;
use crate::config::Config;
use crate::gpu::RenderState;
use crate::net::{NetClient, NetUpdate};
use crate::platform::Instant;
use crate::sim::Sim;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyCommand {
    Movement(Action, bool),
    SelectSlot(usize),
    TogglePerspective,
    ToggleInventory,
    OpenChat { command: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseCommand {
    Motion { dx: i32, dy: i32 },
    Attack(bool),
    Use(bool),
    PickItem { include_data: bool },
    CycleSlot(i32),
}

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
        config.explicit_port(),
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
    Inventory,
}

#[derive(Default)]
struct InventoryUi {
    hovered_slot: Option<usize>,
}

const INVENTORY_SLOT_WIDTH: u16 = 7;

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

fn render_cells(area: Rect) -> Size {
    let area = inner(surface_areas(area).game);
    Size::new(area.width.max(1), area.height.max(1))
}

/// Return the terminal window's physical dimensions when the tty reports them.
///
/// Unix terminals expose these through `TIOCGWINSZ`, but the pixel fields are
/// optional and commonly zero. The tuple is `(columns, rows, pixels wide,
/// pixels high)` so the renderer can scale the game pane by the actual cell
/// geometry instead of assuming a 1:2 character-cell ratio.
fn terminal_pixel_size() -> Option<(u32, u32, u32, u32)> {
    let size = ratatui::crossterm::terminal::window_size().ok()?;
    (size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0).then_some((
        u32::from(size.columns),
        u32::from(size.rows),
        u32::from(size.width),
        u32::from(size.height),
    ))
}

fn render_dimensions(area: Rect, terminal_pixels: Option<(u32, u32, u32, u32)>) -> (u32, u32) {
    let cells = render_cells(area);
    let Some((columns, rows, width, height)) = terminal_pixels else {
        // Halfblocks consumes two source pixels vertically per terminal cell.
        return (u32::from(cells.width), u32::from(cells.height) * 2);
    };

    // Keep one source pixel per physical terminal pixel where the platform
    // reports cell geometry. The halfblock encoder downsamples this frame to
    // `cells.width x cells.height` cells, preserving the camera's physical
    // aspect even when cells are not exactly 1:2.
    let target_width = (u64::from(cells.width) * u64::from(width) + u64::from(columns / 2))
        / u64::from(columns);
    let target_height =
        (u64::from(cells.height) * u64::from(height) + u64::from(rows / 2)) / u64::from(rows);
    (
        u32::try_from(target_width.max(1)).unwrap_or(u32::MAX),
        u32::try_from(target_height.max(1)).unwrap_or(u32::MAX),
    )
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
    let (mut pixel_width, mut pixel_height) =
        render_dimensions(initial_area, terminal_pixel_size());
    let ctx = GpuContext::new_headless_blocking()
        .map_err(|error| anyhow::anyhow!("terminal GPU bring-up failed: {error}"))?;
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, pixel_width, pixel_height, format);
    let mut sim = Sim::new(config.clone());
    sim.connect(config.host.clone(), config.explicit_port(), config.protocol);
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
    let _session = TerminalSession;
    if let Err(error) = execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableFocusChange,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    ) {
        return Err(error.into());
    }

    let mut focus = Focus::Game;
    let mut inventory_ui = InventoryUi::default();
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
                    if !handle_key(
                        key,
                        &mut focus,
                        &mut chat_input,
                        &mut inventory_ui,
                        &mut held,
                        &mut sim,
                    ) {
                        running = false;
                        break;
                    }
                }
                Event::Mouse(mouse) if focus == Focus::Game => {
                    handle_mouse(mouse, inner(areas.game), &mut last_mouse, &mut sim);
                }
                Event::Mouse(mouse) if focus == Focus::Inventory => {
                    handle_inventory_mouse(mouse, inner(areas.game), &mut inventory_ui, &mut sim);
                }
                Event::FocusLost => {
                    reset_terminal_input(&mut held, &mut sim);
                    last_mouse = None;
                }
                Event::FocusGained => last_mouse = None,
                Event::Resize(_, _) => last_mouse = None,
                _ => {}
            }
        }
        if matches!(focus, Focus::Chat | Focus::Inventory) {
            last_mouse = None;
        }
        if !running {
            break;
        }
        expire_unreleased_keys(&mut held, &mut sim);

        let dimensions = render_dimensions(area, terminal_pixel_size());
        if dimensions != (pixel_width, pixel_height) {
            (pixel_width, pixel_height) = dimensions;
            target = HeadlessTarget::new(device, pixel_width, pixel_height, format);
            render.resize(device, pixel_width, pixel_height);
        }

        let now = Instant::now();
        let dt = now.duration_since(last_frame).as_secs_f64().min(0.25);
        last_frame = now;
        sim.step(dt);
        if sim.open_menu().is_some() && focus == Focus::Game {
            reset_terminal_input(&mut held, &mut sim);
            inventory_ui.hovered_slot = None;
            focus = Focus::Inventory;
        }

        for key in sim.drain_removals() {
            render.remove_section(&key);
        }
        for meshed in sim.drain_meshes() {
            render.upload_section(device, queue, meshed.key, &meshed.mesh);
        }

        let aspect = pixel_width as f32 / pixel_height as f32;
        sim.update_target(aspect);
        let camera = sim.render_camera(aspect);
        let frame = target
            .acquire()
            .map_err(|error: TargetError| {
                anyhow::anyhow!("terminal frame acquire failed: {error}")
        })?;
        let entity_draws = sim.entity_draws();
        crate::remote_skins::request_all(
            entity_draws
                .iter()
                .filter_map(|draw| draw.player_skin.as_ref().map(|skin| skin.url.as_str()))
                .chain(
                    entity_draws
                        .iter()
                        .filter_map(|draw| draw.player_skin.as_ref()?.cape.as_deref()),
                ),
        );
        // This resolves the local sheet even in first person, where it feeds the
        // arm; the same source makes the avatar appear when F5 selects a detached
        // camera.
        let body = sim.third_person_body_state();
        render.set_third_person_body_source(move || body.clone());
        render.install_pending_player_skins(device, queue);
        let hand_swing = sim.hand_swing_progress();
        render.set_hand_swing_source(move || hand_swing);
        let item_use = sim.item_use_render_state();
        render.set_item_use_source(move || item_use);
        let hand_bob = sim.bob_frame();
        render.set_hand_bob_source(move || hand_bob);
        let held_item = terminal_main_hand(&sim);
        render.set_main_hand_source(move || held_item.clone());
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
            render_cells(area),
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
            let game_inner = inner(areas.game);
            let hotbar_area = Rect::new(
                game_inner.x,
                game_inner.y.saturating_add(game_inner.height.saturating_sub(1)),
                game_inner.width,
                1,
            );
            frame.render_widget(Paragraph::new(terminal_hotbar(&sim)), hotbar_area);
            if focus == Focus::Inventory {
                let menu = terminal_menu(&sim);
                let panel = inventory_panel(game_inner, menu.slot_count());
                frame.render_widget(Clear, panel);
                frame.render_widget(Block::bordered().title(" Inventory · E or Esc closes "), panel);
                frame.render_widget(
                    Paragraph::new(terminal_inventory_text(&menu, inventory_ui.hovered_slot)),
                    inner(panel),
                );
            }
            let input_title = match focus {
                Focus::Game => " Input · Enter or / to chat · Ctrl-C to quit ",
                Focus::Chat => " Chat · Enter to send · Esc to cancel ",
                Focus::Inventory => " Inventory · click slots · 1–9 swaps hovered slot ",
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
    reset_terminal_input(&mut held, &mut sim);
    Ok(())
}

fn handle_key(
    key: KeyEvent,
    focus: &mut Focus,
    chat_input: &mut String,
    inventory_ui: &mut InventoryUi,
    held: &mut HashMap<Action, Instant>,
    sim: &mut Sim,
) -> bool {
    if key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('c')
    {
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
            if let Some(command) = key_command(key) {
                match command {
                    KeyCommand::Movement(action, pressed) => {
                        sim.input_mut(|input| input.set(action, pressed));
                        if pressed {
                            held.insert(action, Instant::now());
                        } else {
                            held.remove(&action);
                        }
                    }
                    KeyCommand::SelectSlot(slot) => sim.select_slot(slot),
                    KeyCommand::TogglePerspective => sim.cycle_camera_type(),
                    KeyCommand::ToggleInventory => {
                        reset_terminal_input(held, sim);
                        inventory_ui.hovered_slot = None;
                        *focus = Focus::Inventory;
                    }
                    KeyCommand::OpenChat { command } => {
                        reset_terminal_input(held, sim);
                        if command {
                            chat_input.push('/');
                        }
                        *focus = Focus::Chat;
                    }
                }
            }
        }
        Focus::Inventory if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('e' | 'E') => {
                    if sim.open_menu().is_some() {
                        sim.close_open_menu();
                    }
                    inventory_ui.hovered_slot = None;
                    *focus = Focus::Game;
                }
                KeyCode::Char('1'..='9') if inventory_ui.hovered_slot.is_some() => {
                    let KeyCode::Char(key) = key.code else { unreachable!() };
                    let slot = inventory_ui.hovered_slot.expect("guarded above");
                    terminal_menu_click(sim, Click::hotbar_swap(slot, key as u8 - b'1'));
                }
                _ => {}
            }
        }
        _ => {}
    }
    true
}

fn key_command(key: KeyEvent) -> Option<KeyCommand> {
    let pressed = match key.kind {
        KeyEventKind::Press | KeyEventKind::Repeat => true,
        KeyEventKind::Release => false,
    };
    let movement = match key.code {
        KeyCode::Char('w' | 'W') => Some(Action::Forward),
        KeyCode::Char('s' | 'S') => Some(Action::Back),
        KeyCode::Char('a' | 'A') => Some(Action::Left),
        KeyCode::Char('d' | 'D') => Some(Action::Right),
        KeyCode::Char(' ') => Some(Action::Jump),
        KeyCode::Modifier(
            ratatui::crossterm::event::ModifierKeyCode::LeftShift
            | ratatui::crossterm::event::ModifierKeyCode::RightShift,
        ) => Some(Action::Sneak),
        KeyCode::Modifier(
            ratatui::crossterm::event::ModifierKeyCode::LeftControl
            | ratatui::crossterm::event::ModifierKeyCode::RightControl,
        ) => Some(Action::Sprint),
        _ => None,
    };
    if let Some(action) = movement {
        return Some(KeyCommand::Movement(action, pressed));
    }
    if !pressed {
        return None;
    }
    match key.code {
        KeyCode::Char('1'..='9') => {
            let KeyCode::Char(slot) = key.code else { unreachable!() };
            Some(KeyCommand::SelectSlot(usize::from(slot as u8 - b'1')))
        }
        KeyCode::F(5) => Some(KeyCommand::TogglePerspective),
        KeyCode::Char('e' | 'E') => Some(KeyCommand::ToggleInventory),
        KeyCode::Enter | KeyCode::Char('t' | 'T') => {
            Some(KeyCommand::OpenChat { command: false })
        }
        KeyCode::Char('/') => Some(KeyCommand::OpenChat { command: true }),
        _ => None,
    }
}

fn terminal_main_hand(sim: &Sim) -> Option<crate::gpu::MainHandItem> {
    let menu = sim.player_menu();
    let stack = menu.player_native(sim.selected_slot())?;
    let visual = stack.item_model().unwrap_or_else(|| stack.item().clone());
    let item = ResourceLocation::parse(&visual.to_string()).ok()?;
    Some(crate::gpu::MainHandItem {
        item,
        foil: crate::hud::item_icon::stack_has_foil(stack),
        custom_model_data: stack.custom_model_data(),
        dyed_color: stack.dyed_color(),
        potion_color: stack.potion_color(),
        banner_patterns: stack.banner_patterns().to_vec(),
        base_color: stack.base_color().map(str::to_owned),
        skin: crate::hud::item_icon::stack_skin_url(stack),
    })
}

fn terminal_menu(sim: &Sim) -> Menu {
    sim.open_menu()
        .map(|open| open.menu)
        .unwrap_or_else(|| sim.player_menu())
}

fn terminal_hotbar(sim: &Sim) -> String {
    let menu = sim.player_menu();
    (0..9)
        .map(|slot| {
            let label = menu
                .player_native(slot)
                .map_or_else(|| "---".to_owned(), terminal_stack_label);
            let marker = if slot == sim.selected_slot() { '>' } else { ' ' };
            format!("{marker}{}:{label:<3}", slot + 1)
        })
        .collect::<String>()
}

fn terminal_stack_label(stack: &lodestone_game::item::ItemStack) -> String {
    let identifier = stack.item().to_string();
    let name = identifier.rsplit(':').next().unwrap_or("?");
    let mut label = name.chars().take(3).collect::<String>();
    if stack.count() > 1 {
        label = stack.count().min(99).to_string();
    }
    label
}

fn inventory_columns(slot_count: usize) -> usize {
    slot_count.clamp(1, 9)
}

fn inventory_panel(game: Rect, slot_count: usize) -> Rect {
    let columns = inventory_columns(slot_count);
    let rows = slot_count.div_ceil(columns).max(1);
    let width = (u16::try_from(columns).unwrap_or(u16::MAX) * INVENTORY_SLOT_WIDTH)
        .saturating_add(2)
        .min(game.width);
    let height = (u16::try_from(rows).unwrap_or(u16::MAX) + 2).min(game.height);
    Rect::new(
        game.x.saturating_add(game.width.saturating_sub(width) / 2),
        game.y.saturating_add(game.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

fn inventory_slot_at(menu: &Menu, panel: Rect, column: u16, row: u16) -> Option<usize> {
    let content = inner(panel);
    if !content.contains((column, row).into()) {
        return None;
    }
    let columns = inventory_columns(menu.slot_count());
    let local_x = column.saturating_sub(content.x);
    let local_y = row.saturating_sub(content.y);
    let cell_width = (content.width / u16::try_from(columns).ok()?).max(1);
    let slot = usize::from(local_y) * columns + usize::from(local_x / cell_width);
    (slot < menu.slot_count()).then_some(slot)
}

fn terminal_inventory_text(menu: &Menu, hovered: Option<usize>) -> String {
    let columns = inventory_columns(menu.slot_count());
    (0..menu.slot_count())
        .map(|slot| {
            let prefix = if hovered == Some(slot) { '>' } else { ' ' };
            let label = menu
                .slot_item(slot)
                .map_or_else(|| "---".to_owned(), terminal_stack_label);
            format!("{prefix}{slot:02}:{label:<3}")
        })
        .collect::<Vec<_>>()
        .chunks(columns)
        .map(|row| row.concat())
        .collect::<Vec<_>>()
        .join("\n")
}

fn terminal_menu_click(sim: &Sim, click: Click) {
    let Some(net) = sim.net() else { return };
    let shared = net.shared_handle();
    let Some(handle) = shared.get() else { return };
    let _ = handle.menu_click(click, PlayerCtx::survival());
}

fn handle_inventory_mouse(mouse: MouseEvent, game: Rect, ui: &mut InventoryUi, sim: &Sim) {
    let menu = terminal_menu(sim);
    let panel = inventory_panel(game, menu.slot_count());
    let slot = inventory_slot_at(&menu, panel, mouse.column, mouse.row);
    ui.hovered_slot = slot;
    let Some(slot) = slot else { return };
    let click = match mouse.kind {
        MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left)
            if mouse.modifiers.contains(KeyModifiers::SHIFT) => Some(Click::shift(slot)),
        MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left) => Some(Click::left(slot)),
        MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Right) => Some(Click::right(slot)),
        _ => None,
    };
    if let Some(click) = click {
        terminal_menu_click(sim, click);
    }
}

fn mouse_event_command(
    mouse: MouseEvent,
    game: Rect,
    previous: Option<(u16, u16)>,
) -> (Option<MouseCommand>, Option<(u16, u16)>) {
    let position = (mouse.column, mouse.row);
    if !game.contains(position.into()) {
        return (None, None);
    }
    let next = Some(position);
    let command = match mouse.kind {
        MouseEventKind::Moved | MouseEventKind::Drag(_) => previous.map(|(x, y)| {
            MouseCommand::Motion {
                dx: i32::from(mouse.column) - i32::from(x),
                dy: i32::from(mouse.row) - i32::from(y),
            }
        }),
        MouseEventKind::Down(button) => match button {
            ratatui::crossterm::event::MouseButton::Left => Some(MouseCommand::Attack(true)),
            ratatui::crossterm::event::MouseButton::Right => Some(MouseCommand::Use(true)),
            ratatui::crossterm::event::MouseButton::Middle => Some(MouseCommand::PickItem {
                include_data: mouse
                    .modifiers
                    .contains(KeyModifiers::CONTROL),
            }),
        },
        MouseEventKind::Up(button) => match button {
            ratatui::crossterm::event::MouseButton::Left => Some(MouseCommand::Attack(false)),
            ratatui::crossterm::event::MouseButton::Right => Some(MouseCommand::Use(false)),
            ratatui::crossterm::event::MouseButton::Middle => None,
        },
        MouseEventKind::ScrollUp => Some(MouseCommand::CycleSlot(-1)),
        MouseEventKind::ScrollDown => Some(MouseCommand::CycleSlot(1)),
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => None,
    };
    (command, next)
}

fn handle_mouse(
    mouse: MouseEvent,
    game: Rect,
    last_mouse: &mut Option<(u16, u16)>,
    sim: &mut Sim,
) {
    let (command, next) = mouse_event_command(mouse, game, *last_mouse);
    *last_mouse = next;
    match command {
        Some(MouseCommand::Motion { dx, dy }) => {
            sim.input_mut(|input| input.add_mouse(dx as f32, dy as f32));
        }
        Some(MouseCommand::Attack(pressed)) => {
            if pressed {
                sim.begin_attack();
            } else {
                sim.end_attack();
            }
        }
        Some(MouseCommand::Use(pressed)) => {
            if pressed {
                sim.use_item();
            } else {
                sim.end_use();
            }
        }
        Some(MouseCommand::PickItem { include_data }) => sim.pick_block_or_entity(include_data),
        Some(MouseCommand::CycleSlot(delta)) => sim.cycle_slot(delta),
        None => {}
    }
}

fn reset_terminal_input(held: &mut HashMap<Action, Instant>, sim: &mut Sim) {
    held.clear();
    sim.input_mut(|input| input.release_all());
    sim.end_attack();
    sim.end_use();
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
    cells: Size,
) -> anyhow::Result<Protocol> {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = linear_to_srgb_byte(pixel[0]);
        pixel[1] = linear_to_srgb_byte(pixel[1]);
        pixel[2] = linear_to_srgb_byte(pixel[2]);
    }
    let image = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| anyhow::anyhow!("terminal renderer returned an invalid RGBA frame"))?;
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
            DisableFocusChange,
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
        assert_eq!(render_cells(Rect::new(0, 0, 100, 30)), Size::new(66, 25));
        assert_eq!(
            render_dimensions(Rect::new(0, 0, 100, 30), None),
            (66, 50)
        );
    }

    #[test]
    fn terminal_pixels_correct_the_camera_target_aspect_for_non_halfblock_cells() {
        let area = Rect::new(0, 0, 100, 30);
        // A 10x15 physical cell is taller than the halfblock protocol's
        // default 1:2 assumption. The GPU target follows the physical game
        // pane (66*10 by 25*15), while its protocol output remains 66x25
        // cells.
        assert_eq!(
            render_dimensions(area, Some((100, 30, 1_000, 450))),
            (660, 375)
        );
    }

    #[test]
    fn linear_target_colours_are_encoded_for_a_terminal() {
        assert_eq!(linear_to_srgb_byte(0), 0);
        assert_eq!(linear_to_srgb_byte(128), 188);
        assert_eq!(linear_to_srgb_byte(255), 255);
    }

    #[test]
    fn terminal_movement_keys_share_the_controller_actions() {
        assert_eq!(
            key_command(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(KeyCommand::Movement(Action::Forward, true))
        );
        assert_eq!(
            key_command(KeyEvent::new_with_kind(
                KeyCode::Char('D'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            Some(KeyCommand::Movement(Action::Right, false))
        );
        assert_eq!(
            key_command(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            key_command(KeyEvent::new_with_kind(
                KeyCode::Char('w'),
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )),
            Some(KeyCommand::Movement(Action::Forward, true))
        );
        assert_eq!(
            key_command(KeyEvent::new(
                KeyCode::Modifier(
                    ratatui::crossterm::event::ModifierKeyCode::LeftShift,
                ),
                KeyModifiers::NONE,
            )),
            Some(KeyCommand::Movement(Action::Sneak, true))
        );
    }

    #[test]
    fn terminal_hotkeys_select_slots_and_open_chat_or_camera() {
        assert_eq!(
            key_command(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)),
            Some(KeyCommand::SelectSlot(0))
        );
        assert_eq!(
            key_command(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::NONE)),
            Some(KeyCommand::SelectSlot(8))
        );
        assert_eq!(
            key_command(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)),
            Some(KeyCommand::TogglePerspective)
        );
        assert_eq!(
            key_command(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
            Some(KeyCommand::OpenChat { command: false })
        );
        assert_eq!(
            key_command(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
            Some(KeyCommand::OpenChat { command: true })
        );
        assert_eq!(
            key_command(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
            Some(KeyCommand::ToggleInventory)
        );
    }

    #[test]
    fn inventory_grid_maps_cells_to_menu_slots_without_an_out_of_bounds_tail() {
        let menu = Menu::player();
        let panel = inventory_panel(Rect::new(0, 0, 80, 30), menu.slot_count());
        let content = inner(panel);
        assert_eq!(inventory_slot_at(&menu, panel, content.x, content.y), Some(0));
        assert_eq!(
            inventory_slot_at(
                &menu,
                panel,
                content.x.saturating_add(content.width.saturating_sub(1)),
                content.y,
            ),
            Some(8)
        );
        assert_eq!(
            inventory_slot_at(
                &menu,
                panel,
                content.x,
                content.y.saturating_add(6),
            ),
            None,
            "the final partial row must not manufacture a slot"
        );
    }

    #[test]
    fn inventory_text_marks_only_the_hovered_slot() {
        let text = terminal_inventory_text(&Menu::player(), Some(3));
        assert!(text.contains(">03:"));
        assert!(text.contains(" 02:"));
    }

    #[test]
    fn terminal_mouse_events_route_clicks_scroll_and_relative_motion() {
        use ratatui::crossterm::event::MouseButton;

        let game = Rect::new(2, 3, 10, 8);
        let left = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_event_command(left, game, None),
            (Some(MouseCommand::Attack(true)), Some((4, 5)))
        );
        let right = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 4,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_event_command(right, game, Some((4, 5))),
            (Some(MouseCommand::Use(true)), Some((4, 5)))
        );
        let pick = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Middle),
            column: 4,
            row: 5,
            modifiers: KeyModifiers::CONTROL,
        };
        assert_eq!(
            mouse_event_command(pick, game, Some((4, 5))),
            (
                Some(MouseCommand::PickItem { include_data: true }),
                Some((4, 5))
            )
        );
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 7,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_event_command(moved, game, Some((4, 5))),
            (Some(MouseCommand::Motion { dx: 3, dy: -1 }), Some((7, 4)))
        );
        let right_up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Right),
            column: 7,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_event_command(right_up, game, Some((7, 4))),
            (Some(MouseCommand::Use(false)), Some((7, 4)))
        );
        let scroll = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 6,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_event_command(scroll, game, Some((7, 4))),
            (Some(MouseCommand::CycleSlot(-1)), Some((6, 6)))
        );
    }

    #[test]
    fn terminal_mouse_events_outside_the_game_reset_relative_anchor() {
        let outside = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_event_command(outside, Rect::new(2, 3, 10, 8), Some((4, 5))),
            (None, None)
        );
    }

    #[test]
    fn rgba_frame_uses_the_library_halfblock_protocol() {
        let protocol = halfblock_protocol(
            vec![0; 4 * 3 * 4],
            4,
            3,
            Size::new(1, 1),
        )
        .expect("valid RGBA frame");
        assert_eq!(protocol.size(), Size::new(1, 1));
    }
}
