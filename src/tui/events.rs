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
            match event::read()? {
                // Windows also emits Release/Repeat events - handle presses only.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    relayout =
                        handle_key_press(&mut app, key.code, ctrl, config, &outgoing, &mut status);
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
        app.hint_rows(),
    );
    app.set_sidebar_height(g.sidebar.height);
    if app.size() != (g.messages.width, g.messages.height) {
        app.resize(g.messages.width, g.messages.height);
    } else {
        app.sync_view();
    }
}

/// Route one key press to the app; returns whether the screen's layout may
/// have changed (a draft edit or hint completion rewraps the composer).
/// Extracted from `run` so key routing stays unit-testable without a
/// terminal.
fn handle_key_press(
    app: &mut App,
    code: KeyCode,
    ctrl: bool,
    config: &Config,
    outgoing: &std::collections::HashMap<String, ConnectionHandle>,
    status: &mut String,
) -> bool {
    let composer_focused = app.focus() == Focus::Composer;
    match code {
        // Tab completes the highlighted hint while the hint bar is active;
        // otherwise it keeps cycling focus between the panes.
        KeyCode::Tab if composer_focused && app.is_hint_active() => {
            app.complete_hint();
            true
        }
        KeyCode::Tab => {
            app.tab();
            false
        }
        // Up/Down steer the hint highlight while it is active and must not
        // scroll the messages then.
        KeyCode::Up if composer_focused && app.is_hint_active() => {
            app.hint_up();
            false
        }
        KeyCode::Down if composer_focused && app.is_hint_active() => {
            app.hint_down();
            false
        }
        KeyCode::PageUp => {
            app.scroll_page_up();
            false
        }
        KeyCode::PageDown => {
            app.scroll_page_down();
            false
        }
        // Quit: Esc or Ctrl+C.
        KeyCode::Esc => {
            app.quit();
            false
        }
        KeyCode::Char('c') if ctrl => {
            app.quit();
            false
        }
        // j/k navigate the focused pane (or type when composing).
        KeyCode::Char('j') if !ctrl => {
            match app.focus() {
                Focus::Sidebar => app.sidebar_down(),
                Focus::Messages => app.select_next(),
                Focus::Composer => {
                    app.type_char('j');
                    return true;
                }
            }
            false
        }
        KeyCode::Char('k') if !ctrl => {
            match app.focus() {
                Focus::Sidebar => app.sidebar_up(),
                Focus::Messages => app.select_prev(),
                Focus::Composer => {
                    app.type_char('k');
                    return true;
                }
            }
            false
        }
        // Enter interacts with the sidebar row under its cursor,
        // or sends the composer's text to the viewed channel.
        KeyCode::Enter => {
            match app.focus() {
                Focus::Sidebar => app.sidebar_enter(),
                Focus::Composer => {
                    let effect = submit_composer(&mut app.session, config, outgoing, status);
                    app.apply_submission_effect(effect);
                    return true; // the composer may shrink
                }
                Focus::Messages => {}
            }
            false
        }
        // Composer-only editing keys.
        KeyCode::Char(c) if composer_focused && !ctrl && !c.is_control() => {
            app.type_char(c);
            true
        }
        KeyCode::Backspace if composer_focused => {
            app.backspace();
            true
        }
        KeyCode::Delete if composer_focused => {
            app.delete();
            true
        }
        KeyCode::Left if composer_focused => {
            app.cursor_left();
            false
        }
        KeyCode::Right if composer_focused => {
            app.cursor_right();
            false
        }
        KeyCode::Home if composer_focused => {
            app.cursor_home();
            false
        }
        KeyCode::End if composer_focused => {
            app.cursor_end();
            false
        }
        _ => false,
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
        app.hint_rows(),
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

    /// Press one key with no modifiers; returns the relayout flag.
    fn press(app: &mut App, code: KeyCode) -> bool {
        let config = crate::config::Config {
            servers: Default::default(),
            history: Default::default(),
        };
        let outgoing = std::collections::HashMap::new();
        handle_key_press(app, code, false, &config, &outgoing, &mut String::new())
    }

    #[test]
    fn tab_with_an_active_hint_completes_the_highlighted_command_without_cycling_focus() {
        // Arrange: a viewed channel, the composer focused, two hints with
        // the second row highlighted.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.select_buffer(0);
        app.focus_composer();
        app.restore_input("/q".into());
        app.hint_down();
        assert_eq!(app.hint_selected(), 1);

        // Act
        let relayout = press(&mut app, KeyCode::Tab);

        // Assert: the highlighted command completed and focus stayed put.
        assert!(relayout, "completion rewraps the composer");
        assert_eq!(app.input(), "/quit ");
        assert_eq!(app.input_cursor(), 6);
        assert_eq!(app.focus(), Focus::Composer);
        assert!(!app.is_hint_active());
    }

    #[test]
    fn tab_without_a_hint_cycles_focus_as_before() {
        // Arrange: the composer focused with drafts that keep the bar hidden.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.select_buffer(0);
        app.focus_composer();
        for draft in ["", "hello", "/join "] {
            app.restore_input(draft.into());

            // Act
            let relayout = press(&mut app, KeyCode::Tab);

            // Assert: focus moved on without touching the draft.
            assert!(!relayout);
            assert_eq!(app.focus(), Focus::Sidebar, "{draft:?}");
            assert_eq!(app.input(), draft);
            app.focus_composer();
        }
    }

    #[test]
    fn up_and_down_move_the_hint_highlight_without_scrolling_messages() {
        // Arrange: a scrolled history and an active hint bar.
        let mut app = App::new(40, 6);
        app.open_channel("srv", "#a");
        app.select_buffer(0);
        app.focus_composer();
        for index in 0..8 {
            app.session.push_message(crate::core::RoutedMessage::chat(
                "srv",
                "#a",
                "alice",
                &format!("line{index}"),
            ));
        }
        fit_app(&mut app, (60, 8));
        app.set_scroll_offset(0);
        let offset = app.scroll_offset();
        assert!(offset < app.max_offset());
        app.restore_input("/c".into());

        // Act
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);

        // Assert: the highlight moved and clamped, messages did not scroll.
        assert_eq!(app.hint_selected(), 1);
        assert_eq!(
            app.scroll_offset(),
            offset,
            "Up/Down must not scroll messages while the hint bar is active"
        );
        press(&mut app, KeyCode::Up);
        assert_eq!(app.hint_selected(), 0);
        assert_eq!(app.scroll_offset(), offset);
        // With the bar hidden the same keys still do nothing.
        app.restore_input(String::new());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.scroll_offset(), offset);
    }
}
