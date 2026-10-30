//! Terminal event dispatch; application submission has no terminal dependencies.
use super::{App, Focus, StatusZone, dialog::DialogOutcome, mouse, render as ui};
use crate::{
    application::submit_composer,
    config::{self, Config},
    connection::{ConnectionHandle, IrcEvent},
};
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::path::Path;
use std::time::Duration;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MOUSE_SCROLL_LINES: i32 = 3;
pub fn run(
    terminal: &mut ratatui::DefaultTerminal,
    config: &mut Config,
    mut app: App,
    outgoing: &mut std::collections::HashMap<String, ConnectionHandle>,
    tx: std::sync::mpsc::Sender<IrcEvent>,
    rx: std::sync::mpsc::Receiver<IrcEvent>,
    config_path: &Path,
) -> anyhow::Result<()> {
    // First run (no config file, or one without servers): open the
    // setup wizard before the first frame so the user lands in the form.
    if config.first_server().is_none() {
        app.open_config_dialog(config);
    }
    let size = terminal.size()?;
    let mut term_size = (size.width, size.height);
    fit_app(&mut app, term_size);
    let mut status = String::new();
    let started = std::time::Instant::now();
    let theme = config.theme.build()?;

    while app.is_running() {
        fit_app(&mut app, term_size);
        app.tick(started.elapsed());
        let chrome = ui::Chrome {
            status: &status,
            theme: &theme,
        };
        terminal.draw(|frame| ui::draw(frame, &app, &chrome))?;

        if event::poll(POLL_INTERVAL)? {
            let mut relayout = false;
            match event::read()? {
                // Windows also emits Release/Repeat events - handle presses only.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    let alt = key.modifiers.contains(KeyModifiers::ALT);
                    relayout = handle_key_press(
                        &mut app,
                        key.code,
                        ctrl,
                        alt,
                        &mut KeyContext {
                            config,
                            outgoing,
                            tx: &tx,
                            config_path,
                            status: &mut status,
                        },
                    );
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

/// The mutable surroundings one key press may reach: the pieces of the
/// event loop's state that the setup wizard's save path needs, grouped
/// to keep key routing within a sane argument list.
pub(crate) struct KeyContext<'a> {
    config: &'a mut Config,
    outgoing: &'a mut std::collections::HashMap<String, ConnectionHandle>,
    tx: &'a std::sync::mpsc::Sender<IrcEvent>,
    config_path: &'a Path,
    status: &'a mut String,
}

/// Route one key press to the app; returns whether the screen's layout may
/// have changed (a draft edit or hint completion rewraps the composer).
/// Extracted from `run` so key routing stays unit-testable without a
/// terminal.
fn handle_key_press(
    app: &mut App,
    code: KeyCode,
    ctrl: bool,
    alt: bool,
    ctx: &mut KeyContext<'_>,
) -> bool {
    // The setup wizard is modal: while open it consumes every key.
    if app.dialog.is_some() {
        let outcome = app
            .dialog
            .as_mut()
            .map(|dialog| dialog.key(code, ctrl, alt))
            .unwrap_or(DialogOutcome::Edited);
        match outcome {
            DialogOutcome::Cancel => app.close_dialog(),
            DialogOutcome::Save => save_dialog(app, ctx),
            DialogOutcome::Edited => {}
        }
        return false;
    }
    // The welcome page has no buffer, hence no composer, so `/config`
    // cannot be typed there: a plain `c` reopens the wizard instead. With
    // a buffer viewed, `c` is composer input and this never fires; Ctrl+C
    // stays the quit shortcut because `ctrl` is checked.
    if !ctrl && code == KeyCode::Char('c') && app.active_buffer().is_none() {
        app.open_config_dialog(ctx.config);
        return false;
    }
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
                    let effect =
                        submit_composer(&mut app.session, ctx.config, ctx.outgoing, ctx.status);
                    app.apply_submission_effect(effect);
                    // The wizard needs the config, which App does not
                    // hold, so the OpenConfig effect resolves here.
                    if matches!(effect, crate::application::SubmissionEffect::OpenConfig) {
                        app.open_config_dialog(ctx.config);
                    }
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

/// Persist the wizard's form the moment a commit asks for it (WYSIWYG:
/// what the screen shows is the saved state): write the merged config
/// file, sync the in-memory config and the dialog's entry list, and
/// bring the entry live. The worker is respawned only when a
/// connection-affecting field (or the label) changed, so editing a
/// nickname or channels keeps the live connection. The dialog stays
/// open; failures keep it open with the reason on its status row and
/// never discard the committed value.
fn save_dialog(app: &mut App, ctx: &mut KeyContext<'_>) {
    let Some(collected) = app.dialog.as_ref().map(|dialog| dialog.collect()) else {
        return;
    };
    // collect's Err is the i18n key of the violated rule.
    let (name, mut server) = match collected {
        Ok(entry) => entry,
        Err(key) => {
            if let Some(dialog) = app.dialog.as_mut() {
                dialog.error = Some(key);
            }
            return;
        }
    };
    let selected = app
        .dialog
        .as_ref()
        .and_then(|dialog| dialog.selected.clone());
    // A changed label renames the entry: the old section is dropped
    // from the file in the same write and everything keyed by the old
    // label (worker, config, dialog row) moves to the new one.
    let previous_label =
        selected.filter(|old| *old != name && ctx.config.servers.contains_key(old.as_str()));
    // The form owns every key its pages show — password and queries
    // included — so only `proxy`, which no wizard page models, is
    // carried across from the entry being replaced (the renamed old
    // one, or the same-name entry).
    let previous = previous_label
        .as_ref()
        .and_then(|old| ctx.config.servers.get(old))
        .or_else(|| ctx.config.servers.get(&name))
        .cloned();
    if let Some(previous) = &previous {
        server.proxy = previous.proxy.clone();
    }
    if let Err(error) =
        config::save_server(ctx.config_path, previous_label.as_deref(), &name, &server)
    {
        // Log the top-level context only: the error chain can embed raw
        // TOML source lines, which may persist SASL passwords.
        tracing::warn!(target: "termirc::config", "{error}");
        if let Some(dialog) = app.dialog.as_mut() {
            dialog.error = Some("err.cfg_write");
        }
        return;
    }
    // Respawn only when the worker's own view of the server changed: it
    // clones its `ServerConfig` at spawn, so a new host/port/TLS/auth
    // would otherwise wait for a process restart. The label is in the
    // list because the worker map is keyed by it. Nickname, channels
    // and queries ride the live connection instead. Classify new
    // connection fields here.
    let respawn = match &previous {
        None => true,
        Some(previous) => {
            previous_label.is_some()
                || previous.server != server.server
                || previous.port != server.port
                || previous.use_tls != server.use_tls
                || previous.username != server.username
                || previous.password != server.password
                || previous.sasl_username != server.sasl_username
                || previous.sasl_password != server.sasl_password
                || proxies_differ(&previous.proxy, &server.proxy)
        }
    };
    let had_console = app.session.buffers.iter().any(|buffer| {
        buffer.kind == crate::core::BufferKind::Server
            && buffer.server == crate::core::ServerId::new(&name)
    });
    if respawn {
        // Dropping the old handle closes its channels, and the worker's
        // `control.recv()` returning `None` ends its loop.
        if let Some(old) = &previous_label {
            ctx.outgoing.remove(old);
        }
        ctx.outgoing.remove(&name);
        let (_thread, handle) = crate::connection::spawn_irc(
            server.clone(),
            name.clone(),
            server.channels.clone(),
            ctx.tx.clone(),
        );
        ctx.outgoing.insert(name.clone(), handle);
    }
    // Registration is idempotent: an edit keeps its existing buffers
    // and focus, while newly added channels/queries gain theirs.
    app.session.register_server(&name, &server);
    // Land on a respawned/new server's console (idempotent open) so the
    // composer is usable the moment the wizard closes — a commit that
    // kept its live worker keeps the user's focus instead.
    if respawn && !had_console {
        let console = app.session.open_server(&name);
        app.activate_buffer(console);
    }
    // Memory sync: a rename replaces the entry at its original index so
    // file order survives (IndexMap preserves insertion order).
    match &previous_label {
        Some(old) => {
            if let Some(index) = ctx.config.servers.get_index_of(old) {
                ctx.config.servers.shift_remove(old);
                ctx.config
                    .servers
                    .shift_insert(index, name.clone(), server.clone());
            } else {
                ctx.config.servers.insert(name.clone(), server.clone());
            }
        }
        None => {
            ctx.config.servers.insert(name.clone(), server.clone());
        }
    }
    // The dialog's list mirrors the same replacement, keeping the form
    // coherent on the entry it just saved.
    if let Some(dialog) = app.dialog.as_mut() {
        if let Some(old) = &previous_label {
            match dialog.entries.iter().position(|(label, _)| label == old) {
                Some(index) => {
                    dialog.entries.remove(index);
                    dialog.entries.insert(index, (name.clone(), server.clone()));
                }
                None => dialog.entries.push((name.clone(), server.clone())),
            }
            dialog.selected = Some(name.clone());
            dialog.list_cursor = dialog.list_cursor.min(dialog.entries.len() - 1);
        } else if let Some(index) = dialog.entries.iter().position(|(label, _)| *label == name) {
            dialog.entries[index] = (name.clone(), server.clone());
        } else {
            // A first save of the new-server row: the entry joins the
            // list and the form now edits it.
            dialog.entries.push((name.clone(), server.clone()));
            dialog.selected = Some(name.clone());
            dialog.list_cursor = dialog.entries.len() - 1;
        }
        dialog.error = None;
    }
}

/// Proxy equality by value: `ProxyConfig` deliberately derives no
/// `PartialEq` (it is a plain deserialized table), so the respawn
/// decision compares the pieces a worker captures at spawn.
fn proxies_differ(
    left: &Option<crate::config::ProxyConfig>,
    right: &Option<crate::config::ProxyConfig>,
) -> bool {
    match (left, right) {
        (None, None) => false,
        (Some(left), Some(right)) => {
            left.r#type != right.r#type
                || left.host != right.host
                || left.port != right.port
                || left.username != right.username
                || left.password != right.password
        }
        _ => true,
    }
}

/// Route one mouse event: wheel scrolling, click actions, and hover
/// pre-selection (updated on every event - a scroll also moves content
/// under a stationary pointer).
fn handle_mouse(app: &mut App, mouse: MouseEvent, term_size: (u16, u16)) {
    // The setup wizard is modal: mouse events must not reach the panes
    // hidden underneath it.
    if app.dialog.is_some() {
        return;
    }
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
            mouse::MouseTarget::StatusBar => {
                // Clicking a segment focuses its zone: the sidebar on the
                // matching row for server/channel, the composer for input.
                match ui::status_bar_zone_at(app, term_size.0, mouse.column) {
                    Some(StatusZone::Server) => app.focus_sidebar_server(),
                    Some(StatusZone::Channel) => app.focus_sidebar_channel(),
                    Some(StatusZone::Input) => app.focus_composer(),
                    None => {}
                }
            }
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
        let mut app = App::new(40, 5);
        app.open_server("srv");
        let mut queries = Vec::new();
        for index in 0..8 {
            queries.push(app.open_query("srv", &format!("nick{index}")));
        }
        fit_app(&mut app, (70, 5));
        handle_mouse(
            &mut app,
            sidebar_mouse(MouseEventKind::ScrollDown, 1),
            (70, 5),
        );
        assert_eq!(app.sidebar_scroll_offset(), 3);
        assert_eq!(app.sidebar_hovered(), Some(4));
        app.tick(Duration::ZERO);
        assert_eq!(app.sidebar_scroll_offset(), 3);
        handle_mouse(
            &mut app,
            sidebar_mouse(MouseEventKind::Down(MouseButton::Left), 1),
            (70, 5),
        );
        assert_eq!(app.active_buffer().unwrap().id, queries[3]);
        assert_eq!(app.focus(), Focus::Composer);
        handle_mouse(
            &mut app,
            sidebar_mouse(MouseEventKind::ScrollDown, 2),
            (70, 5),
        );
        assert_eq!(app.sidebar_scroll_offset(), 5);
        assert_eq!(app.sidebar_hovered(), Some(7));
        handle_mouse(
            &mut app,
            sidebar_mouse(MouseEventKind::ScrollUp, 0),
            (70, 5),
        );
        assert_eq!(app.sidebar_scroll_offset(), 2);
        assert_eq!(app.sidebar_hovered(), Some(2));
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

    #[test]
    fn status_bar_segment_clicks_focus_their_zones() {
        // Arrange: viewing #a on srv at 70x10 with the sidebar holding
        // its startup focus on the server header row, so the bar paints
        // ` srv ` alone on row 9; the message pane and composer states
        // are reached below.
        let mut app = App::new(70, 10);
        app.open_channel("srv", "#a");
        app.select_buffer(0);
        let bar_click = |column: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 9,
            modifiers: KeyModifiers::NONE,
        };

        // Act / Assert: right of the truncated bar is dead space — this
        // click changes nothing (the sidebar keeps its startup focus).
        handle_mouse(&mut app, bar_click(15), (70, 10));

        // The server block focuses the sidebar on the server header row;
        // its trailing arrow column (col 5) belongs to it too, and the
        // blocks to its right are hidden.
        handle_mouse(&mut app, bar_click(1), (70, 10));
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(app.sidebar_cursor(), Some(0));
        assert_eq!(app.status_zone(), StatusZone::Server);
        handle_mouse(&mut app, bar_click(5), (70, 10));
        assert_eq!(app.status_zone(), StatusZone::Server); // the arrow

        // Where the channel block used to be is now dead space: the click
        // changes nothing. Focus the message pane first (the channel zone)
        // so the channel block is painted again, then click it.
        handle_mouse(&mut app, bar_click(7), (70, 10));
        assert_eq!(app.sidebar_cursor(), Some(0));
        app.focus_messages();
        handle_mouse(&mut app, bar_click(7), (70, 10));
        assert_eq!(app.sidebar_cursor(), Some(1));
        assert_eq!(app.status_zone(), StatusZone::Channel);

        // Columns past the last block are dead space: nothing changes.
        handle_mouse(&mut app, bar_click(25), (70, 10));
        assert_eq!(app.sidebar_cursor(), Some(1));
        assert_eq!(app.status_zone(), StatusZone::Channel);
    }

    /// Press one key with no modifiers; returns the relayout flag.
    fn press(app: &mut App, code: KeyCode) -> bool {
        let mut config = crate::config::Config::default();
        let mut outgoing = std::collections::HashMap::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        let path = std::path::Path::new("/nonexistent/termirc/config.toml");
        let mut ctx = KeyContext {
            config: &mut config,
            outgoing: &mut outgoing,
            tx: &tx,
            config_path: path,
            status: &mut String::new(),
        };
        handle_key_press(app, code, false, false, &mut ctx)
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
        assert_eq!(app.hint_selected(), 2);
        assert_eq!(
            app.scroll_offset(),
            offset,
            "Up/Down must not scroll messages while the hint bar is active"
        );
        press(&mut app, KeyCode::Up);
        assert_eq!(app.hint_selected(), 1);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.hint_selected(), 0);
        assert_eq!(app.scroll_offset(), offset);
        // With the bar hidden the same keys still do nothing.
        app.restore_input(String::new());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.scroll_offset(), offset);
    }

    #[test]
    fn config_command_opens_the_wizard_and_clears_the_draft() {
        // Arrange: a viewed channel with the composer focused.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.select_buffer(0);
        app.focus_composer();
        app.restore_input("/config".into());

        // Act
        press(&mut app, KeyCode::Enter);

        // Assert: the dialog opened and the command left the composer.
        assert!(app.dialog.is_some());
        assert!(app.input().is_empty());
    }

    #[test]
    fn the_open_wizard_consumes_keys_and_escape_closes_it_without_quitting() {
        // Arrange: a draft typed before the wizard opened.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.select_buffer(0);
        app.focus_composer();
        app.restore_input("draft text".into());
        app.open_config_dialog(&crate::config::Config::default());

        // Act: typing is swallowed (fields edit only through Enter),
        // then Enter + typing lands in the focused field (an empty
        // config opens the form directly), never the composer.
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.input(), "draft text");
        assert_eq!(
            app.dialog.as_ref().unwrap().field_text("dlg.field_label"),
            "libera"
        );
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.input(), "draft text");
        assert_eq!(
            app.dialog.as_ref().unwrap().field_text("dlg.field_label"),
            "liberax"
        );
        // Esc discards the open edit instead of closing the dialog...
        press(&mut app, KeyCode::Esc);
        assert!(
            app.dialog.is_some(),
            "Esc discarded the edit, not the dialog"
        );
        assert_eq!(
            app.dialog.as_ref().unwrap().field_text("dlg.field_label"),
            "libera"
        );

        // Act/Assert: Esc closes the dialog instead of quitting the app.
        press(&mut app, KeyCode::Esc);
        assert!(app.dialog.is_none());
        assert!(app.is_running(), "Esc closed the dialog, not the client");

        // A cancelled first run still has an obvious way out: the same
        // Esc that quit before the wizard existed quits now.
        press(&mut app, KeyCode::Esc);
        assert!(!app.is_running(), "Esc on the page still quits");
    }

    #[test]
    fn the_welcome_page_reopens_the_wizard_from_the_c_key() {
        // Arrange: a first-run cancel left the welcome page with no
        // buffer, hence no composer to type /config into.
        let mut app = App::new(40, 10);
        assert!(app.active_buffer().is_none());
        assert!(app.dialog.is_none());

        // Act
        press(&mut app, KeyCode::Char('c'));

        // Assert: the wizard opened and the client kept running.
        assert!(app.dialog.is_some());
        assert!(app.is_running());

        // Ctrl+C is not intercepted by the welcome shortcut: it still
        // quits even with the same empty state.
        let mut bare = App::new(40, 10);
        let mut config = crate::config::Config::default();
        let mut outgoing = std::collections::HashMap::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut ctx = KeyContext {
            config: &mut config,
            outgoing: &mut outgoing,
            tx: &tx,
            config_path: std::path::Path::new("/nonexistent/termirc/config.toml"),
            status: &mut String::new(),
        };
        handle_key_press(&mut bare, KeyCode::Char('c'), true, false, &mut ctx);
        assert!(bare.dialog.is_none());
        assert!(!bare.is_running());
    }

    /// A `KeyContext` over throwaway state, for wizard save tests.
    fn save_context<'a>(
        config: &'a mut crate::config::Config,
        outgoing: &'a mut std::collections::HashMap<String, ConnectionHandle>,
        tx: &'a std::sync::mpsc::Sender<IrcEvent>,
        path: &'a Path,
        status: &'a mut String,
    ) -> KeyContext<'a> {
        KeyContext {
            config,
            outgoing,
            tx,
            config_path: path,
            status,
        }
    }

    #[test]
    fn committing_the_form_writes_the_file_registers_the_server_and_stays_open() {
        // Arrange: a filled wizard pointed at a port that refuses
        // immediately, and a fresh config path.
        let path = std::env::temp_dir().join(format!("termirc-wizard-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut app = App::new(40, 10);
        app.open_config_dialog(&crate::config::Config::default());
        {
            let dialog = app.dialog.as_mut().unwrap();
            dialog.set_field_text("dlg.field_host", "127.0.0.1");
            dialog.set_field_text("dlg.field_port", "1");
            dialog.set_field_text("dlg.field_nickname", "tester");
            dialog.set_field_text("dlg.field_channels", "#a");
            dialog.pane = crate::tui::dialog::Pane::Form;
        }
        let mut config = crate::config::Config::default();
        let mut outgoing = std::collections::HashMap::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut status = String::new();
        let mut ctx = save_context(&mut config, &mut outgoing, &tx, &path, &mut status);

        // Act: Enter opens the focused label field, the second Enter
        // commits — and committing is the save.
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);

        // Assert: the file exists, the config and worker maps gained
        // the entry, the console is viewed — and the dialog stays open
        // with the edit closed (WYSIWYG: keep browsing/editing). Drop
        // the receiver so the background worker's sends fail silently.
        drop(rx);
        assert!(app.dialog.is_some(), "the dialog stays open after a save");
        assert!(!app.dialog.as_ref().unwrap().editing);
        assert!(config.servers.contains_key("libera"));
        assert!(outgoing.contains_key("libera"));
        assert_eq!(
            app.active_buffer().unwrap().kind,
            crate::core::BufferKind::Server
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("server = \"127.0.0.1\""));
        assert!(saved.contains("channels = [\"#a\"]"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn committing_an_edited_server_takes_password_and_queries_from_the_form_and_keeps_proxy() {
        // Arrange: a config file whose entry carries keys some wizard
        // pages model (password, queries) and one no page models
        // (proxy), a live worker handle for it, and a viewed channel.
        let path =
            std::env::temp_dir().join(format!("termirc-wizard-edit-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            "[servers.libera]\nusername = \"me\"\nnickname = \"me\"\npassword = \"keepme\"\nserver = \"old.example.org\"\nuse_tls = false\nport = 6667\nchannels = [\"#one\"]\nqueries = [\"alice\"]\n\n[servers.libera.proxy]\ntype = \"socks5\"\nhost = \"127.0.0.1\"\nport = 9050\n",
        )
        .unwrap();
        let mut config = crate::config::Config::load(&path).unwrap();

        let mut app = App::new(40, 10);
        app.open_server("libera");
        let channel = app.open_channel("libera", "#one");
        app.activate_buffer(channel);
        app.open_config_dialog(&config);
        {
            let dialog = app.dialog.as_mut().unwrap();
            // The host field is where the edit starts; the host change
            // forces a respawn below.
            dialog.pane = crate::tui::dialog::Pane::Form;
            dialog.cursor = crate::tui::dialog::F_HOST;
        }
        // A handle standing in for the worker started at boot. Keeping a
        // clone of its outgoing sender lets the test tell the channels
        // apart; nothing else holds the control receiver, so its closure
        // is observable too.
        let (old_outgoing, _old_outgoing_rx) = tokio::sync::mpsc::channel(8);
        let (old_control, mut old_control_rx) = tokio::sync::mpsc::channel(8);
        let old_sender = old_outgoing.clone();
        let mut outgoing = std::collections::HashMap::from([(
            "libera".to_string(),
            ConnectionHandle {
                outgoing: old_outgoing,
                control: old_control,
            },
        )]);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut status = String::new();
        let mut ctx = save_context(&mut config, &mut outgoing, &tx, &path, &mut status);

        // Act: open the host field, retype it, and commit. The port and
        // the other pages' values are set directly (their own commits
        // would each save just as well).
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);
        for _ in 0.."old.example.org".len() {
            handle_key_press(&mut app, KeyCode::Backspace, false, false, &mut ctx);
        }
        for c in "127.0.0.1".chars() {
            handle_key_press(&mut app, KeyCode::Char(c), false, false, &mut ctx);
        }
        {
            let dialog = app.dialog.as_mut().unwrap();
            dialog.set_field_text("dlg.field_port", "1");
            dialog.set_field_text("dlg.field_password", "");
            dialog.set_field_text("dlg.field_queries", "bob");
        }
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);

        // Assert: the worker was replaced rather than duplicated, and
        // the old control channel closed so its loop ends.
        drop(rx);
        assert!(app.dialog.is_some(), "the dialog stays open");
        assert_eq!(outgoing.len(), 1);
        assert!(
            !outgoing["libera"].outgoing.same_channel(&old_sender),
            "a host change must respawn the server's worker"
        );
        assert!(
            matches!(
                old_control_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
            ),
            "dropping the old handle must close its control channel"
        );
        // An edit keeps the user's buffer focused instead of jumping to
        // the console.
        assert_eq!(app.active_buffer().unwrap().id, channel);

        // Memory agrees with the form: the new connection details, the
        // form's password and queries, and the proxy carried over from
        // the entry the wizard does not model.
        let memory = &config.servers["libera"];
        assert_eq!(memory.server, "127.0.0.1");
        assert_eq!(memory.port, 1);
        assert_eq!(memory.password, "");
        assert_eq!(memory.queries, vec!["bob".to_string()]);
        assert_eq!(memory.proxy.as_ref().map(|proxy| proxy.port), Some(9050));
        // The file gains the new connection details and the form's
        // password and queries (the wizard's writer owns both keys),
        // and keeps the proxy no page models.
        let saved = crate::config::Config::load(&path).unwrap();
        let entry = &saved.servers["libera"];
        assert_eq!(entry.server, "127.0.0.1");
        assert_eq!(entry.port, 1);
        assert_eq!(entry.password, "");
        assert_eq!(entry.queries, vec!["bob".to_string()]);
        assert_eq!(entry.proxy.as_ref().map(|proxy| proxy.port), Some(9050));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_nickname_only_commit_keeps_the_live_worker() {
        // Arrange: one configured entry with a live fake worker and a
        // viewed channel; only the nickname will change.
        let path =
            std::env::temp_dir().join(format!("termirc-wizard-nick-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            "[servers.libera]\nusername = \"me\"\nnickname = \"old\"\nserver = \"irc.example.org\"\nuse_tls = false\nport = 6667\nchannels = [\"#one\"]\n",
        )
        .unwrap();
        let mut config = crate::config::Config::load(&path).unwrap();
        let mut app = App::new(40, 10);
        let channel = app.open_channel("libera", "#one");
        app.activate_buffer(channel);
        app.open_config_dialog(&config);
        {
            let dialog = app.dialog.as_mut().unwrap();
            dialog.pane = crate::tui::dialog::Pane::Form;
            dialog.page = crate::tui::dialog::Page::Identity;
            dialog.cursor = crate::tui::dialog::F_NICKNAME;
        }
        let (old_outgoing, _old_outgoing_rx) = tokio::sync::mpsc::channel(8);
        let (old_control, _old_control_rx) = tokio::sync::mpsc::channel(8);
        let old_sender = old_outgoing.clone();
        let mut outgoing = std::collections::HashMap::from([(
            "libera".to_string(),
            ConnectionHandle {
                outgoing: old_outgoing,
                control: old_control,
            },
        )]);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut status = String::new();
        let mut ctx = save_context(&mut config, &mut outgoing, &tx, &path, &mut status);

        // Act: open the nickname field and commit a change.
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);
        for _ in 0..3 {
            handle_key_press(&mut app, KeyCode::Backspace, false, false, &mut ctx);
        }
        for c in "new".chars() {
            handle_key_press(&mut app, KeyCode::Char(c), false, false, &mut ctx);
        }
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);

        // Assert: the live worker is untouched (same channel, no
        // respawn) while memory and file still gain the new nickname,
        // and the user's buffer keeps focus.
        assert!(app.dialog.is_some());
        assert_eq!(outgoing.len(), 1);
        assert!(
            outgoing["libera"].outgoing.same_channel(&old_sender),
            "a nickname-only commit must keep the live worker"
        );
        assert_eq!(config.servers["libera"].nickname, "new");
        assert_eq!(
            crate::config::Config::load(&path).unwrap().servers["libera"].nickname,
            "new"
        );
        assert_eq!(app.active_buffer().unwrap().id, channel);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn committing_a_renamed_label_replaces_the_entry_and_stops_the_old_worker() {
        // Arrange: two entries, the first with a live fake worker.
        let path =
            std::env::temp_dir().join(format!("termirc-wizard-rename-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            "[servers.alpha]\nusername = \"me\"\nnickname = \"me\"\nserver = \"irc.example.org\"\nuse_tls = false\nport = 6667\nchannels = [\"#one\"]\n\n[servers.beta]\nusername = \"you\"\nnickname = \"you\"\nserver = \"irc2.example.org\"\nuse_tls = false\nport = 6667\nchannels = []\n",
        )
        .unwrap();
        let mut config = crate::config::Config::load(&path).unwrap();
        let mut app = App::new(40, 10);
        app.open_config_dialog(&config);
        app.dialog.as_mut().unwrap().pane = crate::tui::dialog::Pane::Form;
        let (old_outgoing, _old_outgoing_rx) = tokio::sync::mpsc::channel(8);
        let (old_control, mut old_control_rx) = tokio::sync::mpsc::channel(8);
        let mut outgoing = std::collections::HashMap::from([(
            "alpha".to_string(),
            ConnectionHandle {
                outgoing: old_outgoing,
                control: old_control,
            },
        )]);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut status = String::new();
        let mut ctx = save_context(&mut config, &mut outgoing, &tx, &path, &mut status);

        // Act: open the label field, append "2", commit the rename.
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);
        handle_key_press(&mut app, KeyCode::Char('2'), false, false, &mut ctx);
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);

        // Assert: the old worker is stopped and a new one runs under
        // the new label.
        assert!(!outgoing.contains_key("alpha"), "old label worker gone");
        assert!(outgoing.contains_key("alpha2"), "new label worker spawned");
        assert!(
            matches!(
                old_control_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
            ),
            "dropping the old handle must close its control channel"
        );
        // The file, the config map, and the dialog's list all replace
        // the entry at its original position, and the form stays
        // coherent on the renamed entry.
        let saved = crate::config::Config::load(&path).unwrap();
        assert!(!saved.servers.contains_key("alpha"));
        assert_eq!(
            saved.servers.keys().collect::<Vec<_>>(),
            vec!["alpha2", "beta"]
        );
        assert_eq!(
            config.servers.keys().collect::<Vec<_>>(),
            vec!["alpha2", "beta"]
        );
        let dialog = app.dialog.as_ref().unwrap();
        assert_eq!(
            dialog
                .entries
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha2", "beta"]
        );
        assert_eq!(dialog.selected.as_deref(), Some("alpha2"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_commit_with_bad_input_keeps_the_dialog_the_value_and_shows_the_reason() {
        // Arrange: the host field of an existing entry, opened for
        // editing and cleared.
        let mut app = App::new(40, 10);
        app.open_config_dialog(&two_server_config());
        {
            let dialog = app.dialog.as_mut().unwrap();
            dialog.pane = crate::tui::dialog::Pane::Form;
            dialog.cursor = crate::tui::dialog::F_HOST;
        }
        let path = std::env::temp_dir().join("termirc-wizard-invalid.toml");
        let _ = std::fs::remove_file(&path);
        let mut config = crate::config::Config::default();
        let mut outgoing = std::collections::HashMap::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut status = String::new();
        let mut ctx = save_context(&mut config, &mut outgoing, &tx, &path, &mut status);
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);
        for _ in 0.."irc.alpha.org".len() {
            handle_key_press(&mut app, KeyCode::Backspace, false, false, &mut ctx);
        }

        // Act: the commit validates and fails.
        handle_key_press(&mut app, KeyCode::Enter, false, false, &mut ctx);

        // Assert: nothing was written or connected, the reason shows in
        // the status row, and the dialog stays open with the edited
        // value kept (the user must be able to fix it).
        assert!(app.dialog.is_some());
        let dialog = app.dialog.as_ref().unwrap();
        assert_eq!(dialog.error, Some("dlg.err_host"));
        assert!(!dialog.editing);
        assert_eq!(dialog.field_text("dlg.field_host"), "");
        assert!(config.servers.is_empty());
        assert!(outgoing.is_empty());
        assert!(!path.exists());
    }

    /// Two configured servers for wizard tests.
    fn two_server_config() -> crate::config::Config {
        crate::config::Config::parse(
            r##"
[servers.alpha]
username = "me"
nickname = "mynick"
server = "irc.alpha.org"
port = 6667
channels = ["#one"]

[servers.beta]
username = "you"
nickname = "yournick"
server = "irc.beta.org"
port = 7000
channels = ["#three"]
"##,
        )
        .unwrap()
    }
}
