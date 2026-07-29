//! termirc binary: load config, start the IRC thread, run the TUI loop.

use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

use termirc::app::App;
use termirc::config::{Config, ServerConfig};
use termirc::irc::{IrcEvent, spawn_irc};
use termirc::ui;

/// How often the event loop wakes up to pump IRC events and redraw.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Rows/columns taken by the pane border (one on each side).
const BORDER: u16 = 2;

fn main() -> anyhow::Result<()> {
    let config_path = dirs::home_dir()
        .context("could not resolve the home directory")?
        .join(".config")
        .join("termirc")
        .join("config.toml");
    let config = Config::load(&config_path).with_context(|| {
        format!(
            "failed to load config from {} — copy your config file there (e.g. test.toml)",
            config_path.display()
        )
    })?;

    let (server_name, server) = config.first_server().context("config has no servers")?;
    let channel = server
        .first_channel()
        .with_context(|| format!("server '{server_name}' has no channels"))?
        .to_string();

    let mut terminal = ratatui::try_init()?;
    let result = run(&mut terminal, server.clone(), channel);
    ratatui::restore();
    result
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    server: ServerConfig,
    channel: String,
) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    // The IRC thread is deliberately not joined: it blocks on network I/O and
    // is reaped when the process exits.
    let _irc_thread = spawn_irc(server, channel.clone(), tx);

    let size = terminal.size()?;
    let mut app = App::new(
        size.width.saturating_sub(BORDER),
        size.height.saturating_sub(BORDER),
    );
    let mut status = format!("connecting to {channel}…");

    while app.running {
        terminal.draw(|frame| ui::draw(frame, &app, &status))?;

        if event::poll(POLL_INTERVAL)? {
            match event::read()? {
                // Windows also emits Release/Repeat events — handle presses only.
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::PageUp => app.scroll_page_up(),
                    KeyCode::PageDown => app.scroll_page_down(),
                    KeyCode::Char('q') | KeyCode::Esc => app.quit(),
                    _ => {}
                },
                Event::Resize(width, height) => {
                    app.resize(width.saturating_sub(BORDER), height.saturating_sub(BORDER))
                }
                _ => {}
            }
        }

        for irc_event in rx.try_iter() {
            match irc_event {
                IrcEvent::Message(message) => app.push_message(message),
                IrcEvent::Status(text) => status = text,
                IrcEvent::Error(text) => status = format!("error: {text}"),
            }
        }
    }

    Ok(())
}
