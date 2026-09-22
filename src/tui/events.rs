//! Terminal event dispatch; application submission has no terminal dependencies.
use super::{App, Focus, mouse, render as ui};
use crate::{
    application::submit_composer,
    config::Config,
    connection::{ConnectionHandle, IrcEvent},
};
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::time::Duration;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MOUSE_SCROLL_LINES: i32 = 3;
pub fn run(
    terminal: &mut ratatui::DefaultTerminal,
    config: &Config,
    mut app: App,
    outgoing: std::collections::HashMap<String, ConnectionHandle>,
    rx: std::sync::mpsc::Receiver<IrcEvent>,
) -> anyhow::Result<()> {
    let size = terminal.size()?;
    let mut term_size = (size.width, size.height);
    fit_app(&mut app, term_size);
    let mut status = String::new();
    let started = std::time::Instant::now();

    while app.is_running() {
        fit_app(&mut app, term_size);
        app.tick(started.elapsed());
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
                                let effect = submit_composer(
                                    &mut app.session,
                                    config,
                                    &outgoing,
                                    &mut status,
                                );
                                app.apply_submission_effect(effect);
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
            app.session.handle_event(irc_event);
        }
    }

    Ok(())
}

/// Resize the app's message viewport to fit the terminal, leaving room for the
/// (variable-height) composer and the gap below it. The message stream frames
/// itself with separator rows, so no other chrome exists. No-op when the
/// computed size is unchanged.
fn fit_app(app: &mut App, (w, h): (u16, u16)) {
    let g = ui::geometry(
        w,
        h,
        app.input(),
        app.input_cursor(),
        app.active_buffer().is_some(),
    );
    app.set_sidebar_height(g.sidebar.height);
    if app.size() != (g.messages.width, g.messages.height) {
        app.resize(g.messages.width, g.messages.height);
    } else {
        app.sync_view();
    }
}

/// Route one mouse event: wheel scrolling, click actions, and hover
/// pre-selection (updated on every event - a scroll also moves content
/// under a stationary pointer).
fn handle_mouse(app: &mut App, mouse: MouseEvent, term_size: (u16, u16)) {
    let g = ui::geometry(
        term_size.0,
        term_size.1,
        app.input(),
        app.input_cursor(),
        app.active_buffer().is_some(),
    );
    let target = mouse::hit(mouse.column, mouse.row, &g);

    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if matches!(target, mouse::MouseTarget::SidebarRow(_)) {
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -MOUSE_SCROLL_LINES
                } else {
                    MOUSE_SCROLL_LINES
                };
                app.scroll_sidebar(delta);
            } else if matches!(
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
            mouse::MouseTarget::SidebarRow(index) => {
                app.click_sidebar_row(index + app.sidebar_scroll_offset())
            }
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
        mouse::MouseTarget::SidebarRow(index)
            if index + app.sidebar_scroll_offset() < sidebar_len =>
        {
            Some(index + app.sidebar_scroll_offset())
        }
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

    fn sidebar_mouse(kind: MouseEventKind, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: 5,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn sidebar_wheel_hover_and_click_use_visible_offset() {
        let mut app = App::new(40, 3);
        app.open_server("srv");
        let mut queries = Vec::new();
        for index in 0..8 {
            queries.push(app.open_query("srv", &format!("nick{index}")));
        }
        fit_app(&mut app, (70, 3));
        handle_mouse(
            &mut app,
            sidebar_mouse(MouseEventKind::ScrollDown, 1),
            (70, 3),
        );
        assert_eq!(app.sidebar_scroll_offset(), 3);
        assert_eq!(app.sidebar_hovered(), Some(4));
        app.tick(Duration::ZERO);
        assert_eq!(app.sidebar_scroll_offset(), 3);
        handle_mouse(
            &mut app,
            sidebar_mouse(MouseEventKind::Down(MouseButton::Left), 1),
            (70, 3),
        );
        assert_eq!(app.active_buffer().unwrap().id, queries[3]);
        assert_eq!(app.focus(), Focus::Composer);
        handle_mouse(
            &mut app,
            sidebar_mouse(MouseEventKind::ScrollDown, 2),
            (70, 3),
        );
        assert_eq!(app.sidebar_scroll_offset(), 6);
        assert_eq!(app.sidebar_hovered(), Some(8));
        handle_mouse(
            &mut app,
            sidebar_mouse(MouseEventKind::ScrollUp, 0),
            (70, 3),
        );
        assert_eq!(app.sidebar_scroll_offset(), 3);
        assert_eq!(app.sidebar_hovered(), Some(3));
    }

    #[test]
    fn fit_first_selected_query_keeps_newest_and_reads_only_visible_messages() {
        let mut app = App::new(40, 3);
        app.open_server("srv");
        let query = app.open_query("srv", "alice");
        for _ in 0..8 {
            app.session.push_message(crate::core::RoutedMessage {
                server: "srv".into(),
                target: crate::core::BufferKind::Query("alice".into()),
                content: crate::core::MessageContent::chat("alice", "hello"),
            });
        }
        app.session.select_buffer_id(query);
        fit_app(&mut app, (70, 3));
        assert!(app.buffer(query).unwrap().unread);
        fit_app(&mut app, (70, 10));
        assert!(app.is_at_bottom());
        assert!(!app.buffer(query).unwrap().unread);
    }
}
