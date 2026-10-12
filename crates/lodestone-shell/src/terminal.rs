//! Native terminal presentation surfaces.
//!
//! `stdio` is deliberately GPU-free and emits only player-visible text. The
//! `terminal` surface drives the normal [`crate::sim::Sim`], renders its full
//! camera/UI frame through the existing offscreen wgpu target, and hands the
//! RGBA readback to `ratatui-image`'s true-colour Unicode half-block protocol.
//! The only presentation exception is chat: the shared styled spans and editor
//! are composed as readable terminal text over the rasterized game pane, with
//! the prompt in the final terminal row. Neither path creates a window.

#[cfg(any(test, feature = "window"))]
mod surface;
#[cfg(feature = "window")]
pub(crate) use surface::run_terminal;

#[cfg(feature = "multiplayer")]
use std::io;
#[cfg(feature = "multiplayer")]
use std::sync::mpsc::{self, Receiver};
#[cfg(feature = "multiplayer")]
use std::time::Duration;

use crate::config::Config;
#[cfg(feature = "multiplayer")]
use crate::chat::compose_chat_action;
#[cfg(feature = "multiplayer")]
use crate::net::{NetClient, NetUpdate};

#[cfg(feature = "multiplayer")]
enum Input {
    Line(String),
    Closed,
}

#[cfg(feature = "multiplayer")]
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
        for update in net.poll_wait(Duration::from_millis(20)) {
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


#[cfg(not(feature = "window"))]
pub(crate) fn run_terminal(
    _owned: lodestone_auth::Entitlement,
    _config: Config,
) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "the terminal surface requires the window rendering feature in this build"
    ))
}
