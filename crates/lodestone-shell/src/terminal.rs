//! Native terminal presentation surfaces.
//!
//! `stdio` is deliberately GPU-free and emits only player-visible text. The
//! `terminal` surface drives the normal [`crate::sim::Sim`], renders its full
//! camera/UI frame through the existing offscreen wgpu target, and hands the
//! RGBA readback to `ratatui-image`'s true-colour Unicode half-block protocol.
//! The only presentation exception is chat: the shared styled spans and editor
//! are composed as readable terminal text over the rasterized game pane, with
//! the prompt in the final terminal row. Neither path creates a window.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use image::{DynamicImage, RgbaImage};
use lodestone_controller::Action;
use ratatui::crossterm::{
    event::{
        self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui_image::Image;
use ratatui_image::protocol::{Protocol, halfblocks::Halfblocks};

#[cfg(feature = "multiplayer")]
use crate::chat::compose_chat_action;
use crate::config::Config;
#[cfg(feature = "window")]
use crate::app::WindowApp;
#[cfg(feature = "multiplayer")]
use crate::net::{NetClient, NetUpdate};
use crate::platform::Instant;
#[cfg(feature = "window")]
use crate::container::MenuButton;
#[cfg(feature = "window")]
use crate::menu::nav::MenuKey;
use lodestone_model::text::{TextColor, TextSpan};

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
#[cfg(feature = "multiplayer")]
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

#[cfg(not(feature = "multiplayer"))]
pub(crate) fn run_stdio(
    _owned: lodestone_auth::Entitlement,
    _config: Config,
) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "multiplayer is disabled in this build of the game; the stdio surface only joins remote servers"
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Game,
    Chat,
    Inventory,
}

fn current_area() -> io::Result<Rect> {
    let (width, height) = ratatui::crossterm::terminal::size()?;
    Ok(Rect::new(0, 0, width, height))
}

fn terminal_game_area(area: Rect) -> Rect {
    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1).max(1))
}

fn terminal_render_cells(area: Rect) -> Size {
    let game = terminal_game_area(area);
    Size::new(game.width.max(1), game.height.max(1))
}

fn terminal_render_dimensions(
    area: Rect,
    terminal_pixels: Option<(u32, u32, u32, u32)>,
) -> (u32, u32) {
    let cells = terminal_render_cells(area);
    let Some((columns, rows, width, height)) = terminal_pixels else {
        return (u32::from(cells.width), u32::from(cells.height) * 2);
    };
    let target_width = (u64::from(cells.width) * u64::from(width) + u64::from(columns / 2))
        / u64::from(columns);
    let target_height = (u64::from(cells.height) * u64::from(height) + u64::from(rows / 2))
        / u64::from(rows);
    (
        u32::try_from(target_width.max(1)).unwrap_or(u32::MAX),
        u32::try_from(target_height.max(1)).unwrap_or(u32::MAX),
    )
}

/// Convert a terminal cell into the framebuffer coordinate consumed by the
/// shared window/menu hit-testing path. The game pane is the only interactive
/// portion of the image; the prompt row intentionally has no pointer target.
#[cfg(feature = "window")]
fn terminal_pointer_position(
    mouse: MouseEvent,
    game: Rect,
    width: u32,
    height: u32,
) -> Option<(f32, f32)> {
    if !game.contains((mouse.column, mouse.row).into()) {
        return None;
    }
    let local_x = f32::from(mouse.column.saturating_sub(game.x)) + 0.5;
    let local_y = f32::from(mouse.row.saturating_sub(game.y)) + 0.5;
    Some((
        local_x * width as f32 / f32::from(game.width.max(1)),
        local_y * height as f32 / f32::from(game.height.max(1)),
    ))
}

#[cfg(feature = "window")]
fn terminal_menu_button(mouse: MouseEvent) -> Option<MenuButton> {
    match mouse.kind {
        MouseEventKind::Down(button) | MouseEventKind::Up(button) => match button {
            ratatui::crossterm::event::MouseButton::Left => Some(MenuButton::Left),
            ratatui::crossterm::event::MouseButton::Right => Some(MenuButton::Right),
            ratatui::crossterm::event::MouseButton::Middle => Some(MenuButton::Pick),
        },
        _ => None,
    }
}

fn terminal_chat_display(
    entries: &[(Vec<TextSpan>, f32)],
    max_lines: usize,
    chat_open: bool,
) -> Vec<Line<'static>> {
    let mut lines = entries
        .iter()
        .flat_map(|(spans, age)| {
            let alpha = if chat_open { 1.0 } else { terminal_chat_alpha(*age) };
            (alpha > 0.0)
                .then(|| {
                    crate::overlay::spans_lines(spans)
                        .into_iter()
                        .map(move |line| terminal_chat_line(&line, alpha))
                })
                .into_iter()
                .flatten()
        })
        .collect::<Vec<_>>();
    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    lines
}

fn terminal_chat_line(spans: &[TextSpan], alpha: f32) -> Line<'static> {
    Line::from(
        spans
            .iter()
            .map(|span| {
                let mut modifier = Modifier::empty();
                if span.style.bold == Some(true) {
                    modifier.insert(Modifier::BOLD);
                }
                if span.style.italic == Some(true) {
                    modifier.insert(Modifier::ITALIC);
                }
                if span.style.underlined == Some(true) {
                    modifier.insert(Modifier::UNDERLINED);
                }
                if span.style.strikethrough == Some(true) {
                    modifier.insert(Modifier::CROSSED_OUT);
                }
                let rgb = span.style.color.unwrap_or(TextColor::White).rgb();
                Span::styled(
                    span.text.clone(),
                    Style::default()
                        .fg(faded_terminal_color(rgb, alpha))
                        .add_modifier(modifier),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn terminal_chat_alpha(age: f32) -> f32 {
    const CHAT_VISIBLE_SECS: f32 = 10.0;
    const CHAT_FADE_SECS: f32 = 2.0;
    if age <= CHAT_VISIBLE_SECS - CHAT_FADE_SECS {
        1.0
    } else if age >= CHAT_VISIBLE_SECS {
        0.0
    } else {
        (CHAT_VISIBLE_SECS - age) / CHAT_FADE_SECS
    }
}

fn faded_terminal_color(rgb: u32, alpha: f32) -> Color {
    let fade = alpha.clamp(0.0, 1.0);
    Color::Rgb(
        (f32::from(((rgb >> 16) & 0xff) as u8) * fade).round() as u8,
        (f32::from(((rgb >> 8) & 0xff) as u8) * fade).round() as u8,
        (f32::from((rgb & 0xff) as u8) * fade).round() as u8,
    )
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

/// Run the live game in a Ratatui layout with a coloured Unicode image pane.
///
/// The frame is produced by `WindowApp::redraw`, the same entry point used by
/// the winit window. Ratatui is only the final presentation adapter: it reads
/// the completed offscreen framebuffer and converts it to half-block cells.
#[cfg(feature = "window")]
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
        terminal_render_dimensions(initial_area, terminal_pixel_size());
    let mut app = WindowApp::new_terminal(config, pixel_width, pixel_height)?;

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
    let mut last_mouse = None;
    let mut held = HashMap::new();
    let mut last_frame = Instant::now();
    let mut running = true;

    while running {
        let area = current_area()?;
        let game_area = terminal_game_area(area);
        while event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(key) => {
                    app.terminal_set_modifiers(
                        key.modifiers.contains(KeyModifiers::SHIFT),
                        key.modifiers.contains(KeyModifiers::CONTROL),
                    );
                    let before = focus;
                    if !handle_key(key, &mut focus, &mut held, &mut app) {
                        running = false;
                        break;
                    }
                    if before == Focus::Game && focus == Focus::Inventory {
                        app.terminal_toggle_inventory();
                    }
                }
                Event::Mouse(mouse) => {
                    app.terminal_set_modifiers(
                        mouse.modifiers.contains(KeyModifiers::SHIFT),
                        mouse.modifiers.contains(KeyModifiers::CONTROL),
                    );
                    // Crossterm reports terminal cells, while the shared
                    // window path consumes framebuffer pixels. Convert once
                    // and feed the same cursor/menu adapter used by winit;
                    // gameplay relative-look remains based on the raw cells
                    // below so its sensitivity is unchanged.
                    let pointer = terminal_pointer_position(
                        mouse,
                        game_area,
                        pixel_width,
                        pixel_height,
                    );
                    app.terminal_pointer_moved(pointer);
                    if app.terminal_routes_menu_input() {
                        if let Some(button) = terminal_menu_button(mouse) {
                            match mouse.kind {
                                MouseEventKind::Down(_) => app.terminal_pointer_button(button, true),
                                MouseEventKind::Up(_) => app.terminal_pointer_button(button, false),
                                _ => {}
                            }
                        }
                        last_mouse = None;
                    } else if focus == Focus::Game {
                        handle_mouse(mouse, game_area, &mut last_mouse, &mut app);
                    } else if focus == Focus::Inventory {
                        if let MouseEventKind::ScrollUp | MouseEventKind::ScrollDown = mouse.kind {
                            let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) { -1 } else { 1 };
                            app.terminal_cycle_slot(delta);
                        } else if let Some(button) = terminal_menu_button(mouse) {
                            match mouse.kind {
                                MouseEventKind::Down(_) => app.terminal_pointer_button(button, true),
                                MouseEventKind::Up(_) => app.terminal_pointer_button(button, false),
                                _ => {}
                            }
                        }
                        last_mouse = None;
                    }
                }
                Event::FocusLost => {
                    reset_terminal_input(&mut held, &mut app);
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
        expire_unreleased_keys(&mut held, &mut app);

        let dimensions = terminal_render_dimensions(area, terminal_pixel_size());
        if dimensions != (pixel_width, pixel_height) {
            (pixel_width, pixel_height) = dimensions;
            app.resize_terminal(pixel_width, pixel_height);
        }
        let now = Instant::now();
        let _dt = now.duration_since(last_frame).as_secs_f64().min(0.25);
        last_frame = now;
        let pixels = app.redraw_terminal()?;
        let protocol = halfblock_protocol(
            pixels,
            pixel_width,
            pixel_height,
            terminal_render_cells(area),
        )?;
        let chat_lines = app.terminal_chat_lines(
            usize::from(terminal_game_area(area).height.saturating_sub(1)),
        );
        let native_chat = terminal_chat_display(
            &chat_lines,
            usize::from(terminal_game_area(area).height.saturating_sub(1)),
            app.terminal_chat_is_open(),
        );
        terminal.draw(|frame| {
            let area = frame.area();
            let game = terminal_game_area(area);
            frame.render_widget(Image::new(&protocol), game);
            let chat_height = u16::try_from(native_chat.len()).unwrap_or(game.height);
            let chat_y = game.y.saturating_add(game.height.saturating_sub(chat_height));
            frame.render_widget(
                Paragraph::new(native_chat),
                Rect::new(game.x.saturating_add(1), chat_y, game.width.saturating_sub(2), chat_height),
            );
            frame.render_widget(
                Paragraph::new(format!("> {}", app.terminal_chat_text()))
                    .style(Style::default().fg(Color::Yellow)),
                Rect::new(area.x, game.y.saturating_add(game.height), area.width, 1),
            );
        })?;

        std::thread::sleep(Duration::from_millis(80));
    }
    reset_terminal_input(&mut held, &mut app);
    Ok(())
}

#[cfg(not(feature = "window"))]
pub(crate) fn run_terminal(
    _owned: lodestone_auth::Entitlement,
    _config: Config,
) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "the terminal surface requires the window rendering feature in this build"
    ))
}

#[cfg(feature = "window")]
fn handle_key(
    key: KeyEvent,
    focus: &mut Focus,
    held: &mut HashMap<Action, Instant>,
    app: &mut WindowApp,
) -> bool {
    if key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('c')
    {
        return false;
    }

    // Menu navigation is a UI concern, not gameplay input. In particular,
    // this lets a terminal launched without `--host` use the real title-screen
    // rows and actions rather than displaying a non-interactive screenshot.
    if *focus == Focus::Game
        && app.terminal_routes_menu_input()
        && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && let Some(menu_key) = terminal_menu_key(key)
    {
        app.terminal_menu_key(menu_key);
        return true;
    }

    match *focus {
        Focus::Chat if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            match key.code {
                KeyCode::Esc => {
                    app.terminal_chat_cancel();
                    *focus = Focus::Game;
                }
                KeyCode::Enter => {
                    if app.terminal_chat_text().trim() == "#quit" {
                        return false;
                    }
                    app.terminal_chat_submit();
                    *focus = Focus::Game;
                }
                KeyCode::Backspace => {
                    app.terminal_chat_backspace();
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.terminal_chat_push_char(ch);
                }
                KeyCode::Up => {
                    app.terminal_chat_history_up();
                }
                KeyCode::Down => {
                    app.terminal_chat_history_down();
                }
                KeyCode::Left => app.terminal_chat_move_left(),
                KeyCode::Right => app.terminal_chat_move_right(),
                KeyCode::Home => app.terminal_chat_move_start(),
                KeyCode::End => app.terminal_chat_move_end(),
                _ => {}
            }
        }
        Focus::Game => {
            if let Some(command) = key_command(key) {
                match command {
                    KeyCommand::Movement(action, pressed) => {
                        app.terminal_set_action(action, pressed);
                        if pressed {
                            held.insert(action, Instant::now());
                        } else {
                            held.remove(&action);
                        }
                    }
                    KeyCommand::SelectSlot(slot) => app.terminal_select_slot(slot),
                    KeyCommand::TogglePerspective => app.terminal_cycle_camera(),
                    KeyCommand::ToggleInventory => {
                        reset_terminal_input(held, app);
                        *focus = Focus::Inventory;
                    }
                    KeyCommand::OpenChat { command } => {
                        reset_terminal_input(held, app);
                        app.terminal_chat_open(command);
                        *focus = Focus::Chat;
                    }
                }
            }
        }
        Focus::Inventory if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('e' | 'E') => {
                    app.terminal_close_container();
                    *focus = Focus::Game;
                }
                KeyCode::Char('1'..='9') => {
                    let KeyCode::Char(key) = key.code else { unreachable!() };
                    app.terminal_container_hotbar(key as u8 - b'1');
                }
                _ => {}
            }
        }
        _ => {}
    }
    true
}

#[cfg(feature = "window")]
fn terminal_menu_key(key: KeyEvent) -> Option<MenuKey> {
    Some(match key.code {
        KeyCode::Up => MenuKey::Up,
        KeyCode::Down => MenuKey::Down,
        KeyCode::Enter => MenuKey::Enter,
        KeyCode::Esc => MenuKey::Escape,
        KeyCode::Tab => MenuKey::Tab,
        KeyCode::Backspace => MenuKey::Backspace,
        KeyCode::Delete => MenuKey::Delete,
        KeyCode::F(5) => MenuKey::Refresh,
        KeyCode::Char(ch) => MenuKey::Char(ch),
        _ => return None,
    })
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
    app: &mut WindowApp,
) {
    let (command, next) = mouse_event_command(mouse, game, *last_mouse);
    *last_mouse = next;
    match command {
        Some(MouseCommand::Motion { dx, dy }) => {
            app.terminal_mouse_motion(dx as f32, dy as f32);
        }
        Some(MouseCommand::Attack(pressed)) => {
            app.terminal_mouse_attack(pressed);
        }
        Some(MouseCommand::Use(pressed)) => {
            app.terminal_mouse_use(pressed);
        }
        Some(MouseCommand::PickItem { include_data }) => app.terminal_mouse_pick_item(include_data),
        Some(MouseCommand::CycleSlot(delta)) => app.terminal_cycle_slot(delta),
        None => {}
    }
}

fn reset_terminal_input(held: &mut HashMap<Action, Instant>, app: &mut WindowApp) {
    held.clear();
    app.terminal_reset_input();
}

fn expire_unreleased_keys(held: &mut HashMap<Action, Instant>, app: &mut WindowApp) {
    const RELEASE_TIMEOUT: Duration = Duration::from_millis(350);
    let expired = held
        .iter()
        .filter_map(|(action, pressed)| (pressed.elapsed() >= RELEASE_TIMEOUT).then_some(*action))
        .collect::<Vec<_>>();
    for action in expired {
        held.remove(&action);
        app.terminal_expire_action(action);
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
    fn terminal_frame_uses_the_full_game_area_and_reserves_only_prompt_row() {
        let area = Rect::new(0, 0, 120, 40);
        assert_eq!(terminal_game_area(area), Rect::new(0, 0, 120, 39));
        assert_eq!(terminal_render_cells(area), Size::new(120, 39));
        assert_eq!(terminal_render_dimensions(area, None), (120, 78));
    }

    #[test]
    fn native_chat_overlay_preserves_shared_log_fade_ages() {
        assert_eq!(terminal_chat_alpha(0.0), 1.0);
        assert_eq!(terminal_chat_alpha(8.0), 1.0);
        assert_eq!(terminal_chat_alpha(9.0), 0.5);
        assert_eq!(terminal_chat_alpha(10.0), 0.0);
    }

    #[test]
    fn native_chat_overlay_preserves_span_colour_and_formatting() {
        use lodestone_model::text::{TextColor, TextStyle};

        let spans = vec![
            TextSpan {
                text: "red".to_owned(),
                style: TextStyle {
                    color: Some(TextColor::Red),
                    bold: Some(true),
                    italic: Some(false),
                    underlined: Some(true),
                    strikethrough: Some(true),
                    ..TextStyle::default()
                },
            },
            TextSpan {
                text: " hex".to_owned(),
                style: TextStyle {
                    color: Some(TextColor::Rgb(0x12_34_56)),
                    italic: Some(true),
                    ..TextStyle::default()
                },
            },
        ];
        let line = terminal_chat_line(&spans, 1.0);
        assert_eq!(line.spans[0].style.fg, Some(Color::Rgb(255, 85, 85)));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(line.spans[1].style.fg, Some(Color::Rgb(0x12, 0x34, 0x56)));
        assert!(line.spans[1].style.add_modifier.contains(Modifier::ITALIC));
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

    #[cfg(feature = "window")]
    #[test]
    fn terminal_menu_keys_use_the_shared_navigation_protocol() {
        assert_eq!(
            terminal_menu_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Some(MenuKey::Down)
        );
        assert_eq!(
            terminal_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(MenuKey::Enter)
        );
        assert_eq!(
            terminal_menu_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(MenuKey::Char('x'))
        );
        assert_eq!(
            terminal_menu_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            None,
            "unhandled menu keys must remain available to gameplay/chat"
        );
    }

    #[cfg(feature = "window")]
    #[test]
    fn terminal_pointer_maps_cells_to_the_shared_framebuffer_space() {
        let game = Rect::new(4, 2, 10, 5);
        let mouse = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            terminal_pointer_position(mouse, game, 200, 100),
            Some((30.0, 30.0)),
            "one cell is mapped to the centre of its corresponding framebuffer cell"
        );
        assert_eq!(
            terminal_pointer_position(
                MouseEvent { column: 3, ..mouse },
                game,
                200,
                100
            ),
            None
        );
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
