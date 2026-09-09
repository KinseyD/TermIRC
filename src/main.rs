//! termirc binary: load config, start one IRC thread per server, run the TUI.

use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};

use termirc::app::{App, Focus, InputSubmission};
use termirc::command::SlashParseError;
use termirc::config::Config;
use termirc::irc::{IrcEvent, OutgoingMessage, spawn_irc};
use termirc::message::ChatMessage;
use termirc::mouse;
use termirc::ui;

/// How often the event loop wakes up to pump IRC events and redraw.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Lines scrolled per mouse-wheel notch (finer than PgUp/PgDn's third-page).
const MOUSE_SCROLL_LINES: i32 = 3;

fn main() -> anyhow::Result<()> {
    install_panic_hook();

    let home = dirs::home_dir().context("could not resolve the home directory")?;
    let config_path = home.join(".config").join("termirc").join("config.toml");
    let logs_dir = home.join(".config").join("termirc").join("logs");
    // Logging failures are not fatal: run without logs rather than not run.
    if let Err(e) = termirc::logging::init(&logs_dir) {
        eprintln!("logging disabled: {e:#}");
    }
    let config = match Config::load(&config_path) {
        Ok(config) => config,
        Err(e) => {
            let e = e.context(format!(
                "failed to load config from {} - copy your config file there (e.g. test.toml)",
                config_path.display()
            ));
            // Log the top-level context only: the toml error chain embeds
            // the raw offending source line, which could persist a
            // password fragment into the log file.
            tracing::error!("{e}");
            return Err(e);
        }
    };
    if config.first_server().is_none() {
        tracing::error!("config has no servers");
        anyhow::bail!("config has no servers");
    }
    tracing::info!(
        "termirc v{} starting; config={}; servers={}",
        env!("CARGO_PKG_VERSION"),
        config_path.display(),
        config.servers.len()
    );

    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(e) => {
            tracing::error!("failed to initialize terminal: {e:#}");
            return Err(e.into());
        }
    };
    if ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture).is_err() {
        ratatui::restore();
        anyhow::bail!("failed to enable mouse capture");
    }
    let result = run(&mut terminal, &config);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    match &result {
        Ok(()) => tracing::info!("termirc exiting"),
        Err(e) => tracing::error!("termirc exiting on error: {e:#}"),
    }
    ratatui::restore();
    result
}

/// Restore the terminal before the default panic hook prints the backtrace,
/// so a panic never strands the user's shell in raw mode / alternate screen.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
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
    // Register every server's console view and channels in config order;
    // the first view opens when the first message arrives.
    for (name, server) in config.servers.iter() {
        app.open_server(name);
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
                                submit_composer(&mut app, config, &outgoing, &mut status);
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
                Event::Mouse(mouse_event) => handle_mouse(&mut app, mouse_event, term_size),
                _ => {}
            }
            if relayout {
                fit_app(&mut app, term_size);
            }
        }

        for irc_event in rx.try_iter() {
            match irc_event {
                IrcEvent::Message(message) => app.push_message(message),
                // Status changes the status bar AND lands in that server's
                // console view as a nick-less line.
                IrcEvent::Status(server, text) | IrcEvent::Error(server, text) => {
                    status = format!("{server}: {text}");
                    app.push_message(ChatMessage {
                        server,
                        channel: String::new(),
                        nick: String::new(),
                        text,
                    });
                }
            }
        }
    }

    Ok(())
}

/// Handle Enter: slash input produces only debug logs; ordinary input sends
/// and echoes locally. Parsing feedback never changes the UI status.
fn submit_composer(
    app: &mut App,
    config: &Config,
    outgoing: &std::collections::HashMap<String, tokio::sync::mpsc::Sender<OutgoingMessage>>,
    status: &mut String,
) {
    let out = match app.submit_input() {
        Some(InputSubmission::Outgoing(out)) => out,
        Some(InputSubmission::Slash(_)) => {
            tracing::debug!(target: "termirc::slash", outcome = "parsed",
                "slash input parsed; execution disabled");
            return;
        }
        Some(InputSubmission::InvalidSlash(SlashParseError::MissingName)) => {
            tracing::debug!(target: "termirc::slash", outcome = "rejected",
                reason = "missing_name", "slash input parse failed");
            return;
        }
        None => return,
    };
    let (server, channel, text) = match &out {
        OutgoingMessage::Privmsg {
            server,
            target,
            text,
        } => (server.clone(), target.clone(), text.clone()),
        // Raw lines echo into the server's console (empty-channel sentinel).
        OutgoingMessage::Raw { server, line } => (server.clone(), String::new(), line.clone()),
    };
    let nickname = nickname_of(config, &server);
    match outgoing.get(&server).map(|s| s.try_send(out)) {
        Some(Ok(())) => {
            app.push_message(ChatMessage {
                server,
                channel,
                nick: nickname,
                text,
            });
        }
        _ => {
            app.restore_input(text);
            tracing::warn!("send failed on {server}: disconnected or busy");
            *status = format!("failed to send ({server} disconnected or busy)");
        }
    }
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

/// Route one mouse event: wheel scrolling, click actions, and hover
/// pre-selection (updated on every event - a scroll also moves content
/// under a stationary pointer).
fn handle_mouse(app: &mut App, mouse: MouseEvent, term_size: (u16, u16)) {
    let (_, input_text_width) = ui::input_text_geometry(term_size.0);
    let input_lines =
        termirc::layout::input_line_count(app.input(), app.input_cursor(), input_text_width);
    // The welcome page has no composer; its message pane owns the height.
    let composer_rows = if app.active_channel().is_some() {
        ui::composer_height(input_lines)
    } else {
        0
    };
    let target = mouse::hit(
        mouse.column,
        mouse.row,
        term_size.0,
        term_size.1,
        composer_rows,
    );

    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if matches!(
                target,
                mouse::MouseTarget::MessageRow(_) | mouse::MouseTarget::MessageBlank
            ) {
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -MOUSE_SCROLL_LINES
                } else {
                    MOUSE_SCROLL_LINES
                };
                app.scroll_lines(delta);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => match target {
            mouse::MouseTarget::SidebarRow(i) => app.click_sidebar_row(i),
            mouse::MouseTarget::MessageRow(row) => match app.message_at_row(row) {
                Some(index) => app.click_message(index),
                None => app.focus_messages(),
            },
            mouse::MouseTarget::MessageBlank => app.focus_messages(),
            mouse::MouseTarget::Composer => app.focus_composer(),
            mouse::MouseTarget::None => {}
        },
        _ => {}
    }

    // Hover follows the pointer (bounds-checked against the row list).
    let sidebar_len = app.sidebar_rows().len();
    app.set_hover_sidebar(match target {
        mouse::MouseTarget::SidebarRow(i) if i < sidebar_len => Some(i),
        _ => None,
    });
    app.set_hover_message(match target {
        mouse::MouseTarget::MessageRow(row) => app.message_at_row(row),
        _ => None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn slash_submissions_never_send_echo_or_execute() {
        for console in [false, true] {
            for input in [
                "/join #new",
                "/quit",
                "/raw JOIN #new",
                "/unknown a b",
                "/MSG alice hello  世界",
                "  /nick newname  ",
                "//hello",
                "/private-command-name sensitive-payload-260909",
                "/",
                "/   ",
                "/ join",
            ] {
                let mut app = App::new(40, 10);
                if console {
                    app.open_server("srv");
                } else {
                    app.open_channel("srv", "#a");
                }
                app.select_channel(0);
                for c in input.chars() {
                    app.type_char(c);
                }
                let old_cursor = app.input_cursor();
                let config = Config {
                    servers: Default::default(),
                };
                let (tx, mut rx) = tokio::sync::mpsc::channel(8);
                let outgoing = std::collections::HashMap::from([("srv".to_string(), tx)]);
                let mut status = "previous connection status".to_string();
                let capture = LogCapture::default();
                let writer = capture.clone();
                let subscriber = tracing_subscriber::fmt()
                    .with_ansi(false)
                    .with_max_level(tracing::Level::DEBUG)
                    .with_writer(move || writer.clone())
                    .finish();

                tracing::subscriber::with_default(subscriber, || {
                    submit_composer(&mut app, &config, &outgoing, &mut status);
                });
                let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();

                assert!(
                    matches!(
                        rx.try_recv(),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    ),
                    "slash input entered outgoing queue: {input:?}, console={console}"
                );
                assert!(app.messages().is_empty());
                assert!(app.is_running());
                assert_eq!(app.channel_count(), 1);
                assert_eq!(status, "previous connection status");
                assert_eq!(logs.lines().count(), 1);
                assert!(logs.contains("DEBUG"));
                assert!(logs.contains("termirc::slash"));
                assert!(!logs.contains(input.trim()));
                assert!(!logs.contains("private-command-name"));
                assert!(!logs.contains("sensitive-payload-260909"));
                if matches!(input, "/" | "/   " | "/ join") {
                    assert_eq!(app.input(), input);
                    assert_eq!(app.input_cursor(), old_cursor);
                    assert!(logs.contains("rejected"));
                    assert!(logs.contains("missing_name"));
                } else {
                    assert_eq!(app.input(), "");
                    assert_eq!(app.input_cursor(), 0);
                    assert!(logs.contains("parsed"));
                }
            }
        }
    }

    #[test]
    fn ordinary_submissions_send_echo_and_restore_on_failure() {
        for console in [false, true] {
            for connected in [false, true] {
                let input = if console { "WHOIS nick" } else { "hello /join" };
                let mut app = App::new(40, 10);
                if console {
                    app.open_server("srv");
                } else {
                    app.open_channel("srv", "#a");
                }
                app.select_channel(0);
                for c in input.chars() {
                    app.type_char(c);
                }
                let config = Config {
                    servers: Default::default(),
                };
                let (tx, mut rx) = tokio::sync::mpsc::channel(8);
                let mut outgoing = std::collections::HashMap::new();
                if connected {
                    outgoing.insert("srv".to_string(), tx);
                }
                let mut status = String::new();
                submit_composer(&mut app, &config, &outgoing, &mut status);
                if connected {
                    let expected = if console {
                        OutgoingMessage::Raw {
                            server: "srv".into(),
                            line: input.into(),
                        }
                    } else {
                        OutgoingMessage::Privmsg {
                            server: "srv".into(),
                            target: "#a".into(),
                            text: input.into(),
                        }
                    };
                    assert_eq!(rx.try_recv().unwrap(), expected);
                    assert_eq!(app.messages().len(), 1);
                    assert_eq!(app.messages()[0].text, input);
                    assert_eq!(app.input(), "");
                } else {
                    assert!(app.messages().is_empty());
                    assert_eq!(app.input(), input);
                    assert!(status.contains("failed to send"));
                }
            }
        }
    }
}
