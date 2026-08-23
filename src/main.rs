//! termirc binary: load config, start one IRC thread per server, run the TUI.

use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use termirc::app::{App, Focus};
use termirc::config::Config;
use termirc::irc::{IrcEvent, OutgoingMessage, spawn_irc};
use termirc::message::ChatMessage;
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
    if config.first_server().is_none() {
        anyhow::bail!("config has no servers");
    }

    let mut terminal = ratatui::try_init()?;
    let result = run(&mut terminal, &config);
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

fn run(terminal: &mut ratatui::DefaultTerminal, config: &Config) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    // One IRC thread per configured server, joining all of its channels, plus
    // the sender half of each connection's outgoing-message channel.
    // Threads are deliberately not joined: they block on network I/O and are
    // reaped when the process exits.
    let mut outgoing: std::collections::HashMap<
        String,
        tokio::sync::mpsc::Sender<OutgoingMessage>,
    > = std::collections::HashMap::new();
    for (name, server) in config.servers.iter() {
        let (_handle, sender) = spawn_irc(
            server.clone(),
            name.clone(),
            server.channels.clone(),
            tx.clone(),
        );
        outgoing.insert(name.clone(), sender);
    }
    drop(tx);

    let s = terminal.size()?;
    let mut term_size = (s.width, s.height);
    let mut app = App::new(1, 1);
    // Register every server's channels in config order; the first is viewed.
    for (name, server) in config.servers.iter() {
        for channel in &server.channels {
            app.open_channel(name, channel);
        }
    }
    fit_app(&mut app, term_size);
    let mut status = "connecting…".to_string();

    while app.is_running() {
        let chrome = ui::Chrome { status: &status };
        terminal.draw(|frame| ui::draw(frame, &app, &chrome))?;

        if event::poll(POLL_INTERVAL)? {
            let mut relayout = false;
            let composer_focused = app.focus() == Focus::Composer;
            match event::read()? {
                // Windows also emits Release/Repeat events - handle presses only.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Tab => app.tab(),
                        KeyCode::PageUp => app.scroll_page_up(),
                        KeyCode::PageDown => app.scroll_page_down(),
                        // Quit: Esc or Ctrl+C.
                        KeyCode::Esc => app.quit(),
                        KeyCode::Char('c') if ctrl => app.quit(),
                        // j/k navigate the focused pane (or type when composing).
                        KeyCode::Char('j') if !ctrl => match app.focus() {
                            Focus::Sidebar => app.sidebar_down(),
                            Focus::Messages => app.select_next(),
                            Focus::Composer => {
                                app.type_char('j');
                                relayout = true;
                            }
                        },
                        KeyCode::Char('k') if !ctrl => match app.focus() {
                            Focus::Sidebar => app.sidebar_up(),
                            Focus::Messages => app.select_prev(),
                            Focus::Composer => {
                                app.type_char('k');
                                relayout = true;
                            }
                        },
                        // Enter interacts with the sidebar row under its cursor,
                        // or sends the composer's text to the viewed channel.
                        KeyCode::Enter => match app.focus() {
                            Focus::Sidebar => app.sidebar_enter(),
                            Focus::Composer => {
                                if let Some(out) = app.submit_input() {
                                    let nickname = nickname_of(config, &out.server);
                                    match outgoing.get(&out.server).map(|s| s.try_send(out.clone()))
                                    {
                                        Some(Ok(())) => {
                                            // Echo our own line locally (IRC
                                            // servers do not send it back).
                                            app.push_message(ChatMessage {
                                                server: out.server,
                                                channel: out.target,
                                                nick: nickname,
                                                text: out.text,
                                            });
                                        }
                                        _ => {
                                            // Connection gone or queue full:
                                            // put the text back, explain.
                                            app.restore_input(out.text);
                                            status = format!(
                                                "failed to send ({} disconnected or busy)",
                                                out.server
                                            );
                                        }
                                    }
                                }
                                relayout = true; // the composer may shrink
                            }
                            Focus::Messages => {}
                        },
                        // Composer-only editing keys.
                        KeyCode::Char(c) if composer_focused && !ctrl && !c.is_control() => {
                            app.type_char(c);
                            relayout = true;
                        }
                        KeyCode::Backspace if composer_focused => {
                            app.backspace();
                            relayout = true;
                        }
                        KeyCode::Delete if composer_focused => {
                            app.delete();
                            relayout = true;
                        }
                        KeyCode::Left if composer_focused => app.cursor_left(),
                        KeyCode::Right if composer_focused => app.cursor_right(),
                        KeyCode::Home if composer_focused => app.cursor_home(),
                        KeyCode::End if composer_focused => app.cursor_end(),
                        _ => {}
                    }
                }
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
                IrcEvent::Error(text) => status = text,
            }
        }
    }

    Ok(())
}

/// The configured nickname for a server (matched case-insensitively by its
/// config key), for echoing our own sent messages.
fn nickname_of(config: &Config, server: &str) -> String {
    config
        .servers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(server))
        .map(|(_, server)| server.nickname.clone())
        .unwrap_or_else(|| server.to_string())
}

/// Resize the app's message viewport to fit the terminal, leaving room for the
/// (variable-height) composer and the gap below it. The message stream frames
/// itself with separator rows, so no other chrome exists. No-op when the
/// computed size is unchanged.
fn fit_app(app: &mut App, (w, h): (u16, u16)) {
    let message_width = w
        .saturating_sub(ui::SIDEBAR_WIDTH)
        .saturating_sub(ui::SEPARATOR_GAP)
        .saturating_sub(2 * ui::HORIZONTAL_PAD);
    let (_, input_text_width) = ui::input_text_geometry(w);
    let input_lines =
        termirc::layout::input_line_count(app.input(), app.input_cursor(), input_text_width);
    let message_height = h
        .saturating_sub(ui::composer_height(input_lines))
        .saturating_sub(ui::GAP_ROWS);
    if app.size() != (message_width, message_height) {
        app.resize(message_width, message_height);
    }
}
