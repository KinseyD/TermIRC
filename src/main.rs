//! termirc binary: load config, start the IRC thread, run the TUI loop.

use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use termirc::app::App;
use termirc::config::{Config, ServerConfig};
use termirc::irc::{IrcEvent, spawn_irc};
use termirc::ui;

/// How often the event loop wakes up to pump IRC events and redraw.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

fn main() -> anyhow::Result<()> {
    install_panic_hook();

    let config_path = dirs::home_dir()
        .context("could not resolve the home directory")?
        .join(".config")
        .join("termirc")
        .join("config.toml");
    let config = Config::load(&config_path).with_context(|| {
        format!(
            "failed to load config from {} - copy your config file there (e.g. test.toml)",
            config_path.display()
        )
    })?;

    let (server_name, server) = config.first_server().context("config has no servers")?;
    let channel = server
        .first_channel()
        .with_context(|| format!("server '{server_name}' has no channels"))?
        .to_string();

    let mut terminal = ratatui::try_init()?;
    let result = run(&mut terminal, server.clone(), channel, &config, server_name);
    ratatui::restore();
    result
}

/// Restore the terminal before the default panic hook prints the backtrace,
/// so a panic never strands the user's shell in raw mode / alternate screen.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ratatui::try_restore();
        default_hook(info);
    }));
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    server: ServerConfig,
    channel: String,
    config: &Config,
    server_name: &str,
) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    // The IRC thread is deliberately not joined: it blocks on network I/O and
    // is reaped when the process exits.
    let _irc_thread = spawn_irc(server, channel.clone(), tx);

    let size = terminal.size()?;
    let mut app = App::new(
        size.width
            .saturating_sub(ui::SIDEBAR_WIDTH)
            .saturating_sub(2 * ui::HORIZONTAL_PAD),
        size.height
            .saturating_sub(ui::TITLE_ROWS)
            .saturating_sub(ui::INPUT_ROWS),
    );
    let mut status = format!("connecting to {channel}…");

    while app.is_running() {
        let chrome = ui::Chrome {
            config,
            active_server: server_name,
            active_channel: &channel,
            status: &status,
        };
        terminal.draw(|frame| ui::draw(frame, &app, &chrome))?;

        if event::poll(POLL_INTERVAL)? {
            match event::read()? {
                // Windows also emits Release/Repeat events - handle presses only.
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::PageUp => app.scroll_page_up(),
                    KeyCode::PageDown => app.scroll_page_down(),
                    KeyCode::Char('q') | KeyCode::Esc => app.quit(),
                    // Raw mode disables SIGINT, so Ctrl+C arrives as a key event.
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.quit()
                    }
                    _ => {}
                },
                Event::Resize(width, height) => app.resize(
                    width
                        .saturating_sub(ui::SIDEBAR_WIDTH)
                        .saturating_sub(2 * ui::HORIZONTAL_PAD),
                    height
                        .saturating_sub(ui::TITLE_ROWS)
                        .saturating_sub(ui::INPUT_ROWS),
                ),
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
