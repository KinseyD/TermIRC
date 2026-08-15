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
    // IRC threads are deliberately not joined: they block on network I/O and
    // are reaped when the process exits.
    let channels = server.channels.clone();
    let _irc_thread = spawn_irc(server, server_name.to_string(), channels, tx);

    let s = terminal.size()?;
    let mut term_size = (s.width, s.height);
    let mut app = App::new(1, 1);
    fit_app(&mut app, term_size);
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
            let mut relayout = false;
            match event::read()? {
                // Windows also emits Release/Repeat events - handle presses only.
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::PageUp => app.scroll_page_up(),
                    KeyCode::PageDown => app.scroll_page_down(),
                    // Quit: Esc or Ctrl+C. (`q` now types into the composer.)
                    KeyCode::Esc => app.quit(),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.quit()
                    }
                    // Composer input (receive-only: Enter does not send). Any
                    // edit can change the composer's line count, so re-fit.
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL) && !c.is_control() =>
                    {
                        app.type_char(c);
                        relayout = true;
                    }
                    KeyCode::Backspace => {
                        app.backspace();
                        relayout = true;
                    }
                    KeyCode::Delete => {
                        app.delete();
                        relayout = true;
                    }
                    KeyCode::Left => {
                        app.cursor_left();
                        relayout = true;
                    }
                    KeyCode::Right => {
                        app.cursor_right();
                        relayout = true;
                    }
                    KeyCode::Home => {
                        app.cursor_home();
                        relayout = true;
                    }
                    KeyCode::End => {
                        app.cursor_end();
                        relayout = true;
                    }
                    KeyCode::Enter => {}
                    _ => {}
                },
                Event::Resize(width, height) => {
                    term_size = (width, height);
                    relayout = true;
                }
                _ => {}
            }
            if relayout {
                fit_app(&mut app, term_size);
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

/// Resize the app's message viewport to fit the terminal, leaving room for the
/// title, the message/composer spacer, the (variable-height) composer, and the
/// gap below it. No-op when the computed size is unchanged.
fn fit_app(app: &mut App, (w, h): (u16, u16)) {
    let message_width = w
        .saturating_sub(ui::SIDEBAR_WIDTH)
        .saturating_sub(ui::SEPARATOR_GAP)
        .saturating_sub(2 * ui::HORIZONTAL_PAD);
    let (_, input_text_width) = ui::input_text_geometry(w);
    let input_lines =
        termirc::layout::input_line_count(app.input(), app.input_cursor(), input_text_width);
    let message_height = h
        .saturating_sub(ui::TITLE_ROWS)
        .saturating_sub(ui::MESSAGE_INPUT_SPACER)
        .saturating_sub(ui::composer_height(input_lines))
        .saturating_sub(ui::GAP_ROWS);
    if app.size() != (message_width, message_height) {
        app.resize(message_width, message_height);
    }
}
