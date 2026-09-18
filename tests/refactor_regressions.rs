use termirc::{core::RoutedMessage, tui::App};

fn msg(i: usize) -> RoutedMessage {
    RoutedMessage::chat("srv", "#a", "n", &format!("{i}"))
}

#[test]
fn narrowing_never_deletes_history() {
    let mut app = App::new(80, 20);
    app.open_channel("srv", "#a");
    for i in 0..120 {
        let mut message = msg(i);
        message.content.text = "x".repeat(500);
        app.push_message(message);
    }
    app.select_buffer(0);
    app.resize(1, 20);
    assert_eq!(app.messages().len(), 120);
    assert!(app.total_height() > 50_000);
}

#[test]
fn history_eviction_keeps_the_reading_message() {
    let mut app = App::new(80, 20);
    app.open_channel("srv", "#a");
    for i in 0..5000 {
        app.push_message(msg(i));
    }
    app.select_buffer(0);
    app.set_scroll_offset(201);
    let before = app.visible_lines()[0].body.clone();
    app.push_message(msg(5000));
    assert_eq!(app.visible_lines()[0].body, before);
}

#[test]
fn widening_keeps_the_anchored_message_visible_when_its_old_row_disappears() {
    let mut app = App::new(5, 3);
    app.push_message(RoutedMessage::chat("srv", "#a", "", &"a".repeat(50)));
    let anchored = app.messages()[0].id;
    for _ in 0..10 {
        app.push_message(RoutedMessage::chat("srv", "#a", "", "later"));
    }
    app.set_scroll_offset(7);
    app.resize(80, 3);
    let index = app
        .message_at_row(0)
        .expect("reading message must stay on screen");
    assert_eq!(app.messages()[index].id, anchored);
    assert_eq!(app.visible_lines()[0].body, "a".repeat(50));
}

#[test]
fn buffer_switch_restores_its_own_draft_and_cursor() {
    let mut app = App::new(80, 20);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");
    app.select_buffer(0);
    app.restore_input_at("draft a".into(), 2);
    app.select_buffer(1);
    assert_eq!(app.input(), "");
    app.restore_input_at("draft b".into(), 3);
    app.select_buffer(0);
    assert_eq!(app.input(), "draft a");
    assert_eq!(app.input_cursor(), 2);
}

#[test]
fn chinese_composer_wraps_by_display_width() {
    assert_eq!(
        termirc::tui::layout::input_line_count("一二三四五六七八九十", 10, 13),
        2
    );
    let mixed = termirc::tui::input::InputLayout::new("你好 hello世界!!", 9, 13);
    assert_eq!(mixed.lines[0].text, "你好 hello世");
    assert_eq!(mixed.lines[1].text, "界!!");
    assert_eq!((mixed.cursor_row, mixed.cursor_column), (1, 0));
    let end = termirc::tui::input::InputLayout::new("你好 hello世界!!", 12, 13);
    assert_eq!((end.cursor_row, end.cursor_column), (1, 4));
}

#[test]
fn history_beyond_u16_range_supports_scroll_selection_and_hit_testing() {
    let mut app = App::new(80, 20);
    app.open_channel("srv", "#a");
    for _ in 0..150 {
        app.push_message(RoutedMessage::chat("srv", "#a", "n", &"x".repeat(500)));
    }
    app.select_buffer(0);
    app.resize(1, 20);
    assert!(app.total_height() > 65_535);
    app.set_scroll_offset(70_000);
    let index = app.message_at_row(0).unwrap();
    assert_eq!(index, 139);
    app.click_message(index);
    assert_eq!(app.selected(), Some(139));
    app.select_next();
    assert_eq!(app.selected(), Some(140));
    assert!(app.scroll_offset() > 65_535);
}

#[test]
fn chinese_input_renders_every_character_and_tiny_windows_do_not_panic() {
    use ratatui::{Terminal, backend::TestBackend};
    use termirc::tui::render::{self, Chrome};
    let mut app = App::new(16, 13);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    app.focus_composer();
    for c in "一二三四五六七八九十".chars() {
        app.type_char(c);
    }
    let mut terminal = Terminal::new(TestBackend::new(44, 20)).unwrap();
    terminal
        .draw(|f| render::draw(f, &app, &Chrome { status: "" }))
        .unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    for c in "一二三四五六七八九十".chars() {
        assert!(text.contains(c), "missing {c}");
    }
    for w in 0..35 {
        for h in 0..8 {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            let g = render::geometry(w, h, app.input(), app.input_cursor(), true);
            app.resize(g.messages.width, g.messages.height);
            terminal
                .draw(|f| render::draw(f, &app, &Chrome { status: "" }))
                .unwrap();
        }
    }
}

#[test]
fn clipped_composer_keeps_cursor_row_visible_and_mouse_geometry_matches() {
    use ratatui::{Terminal, backend::TestBackend, style::Color};
    use termirc::tui::{
        mouse::{self, MouseTarget},
        render::{self, Chrome},
    };
    let mut app = App::new(16, 13);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    app.focus_composer();
    app.restore_input("一二三四五六七八九十".into());
    for height in 1..9 {
        let g = render::geometry(44, height, app.input(), app.input_cursor(), true);
        assert!(g.input.height > 0, "no input row at height {height}");
        let cursor_x = g.input.x + g.input_layout.cursor_column as u16;
        let cursor_y = g.input.y + (g.input_layout.cursor_row - g.first_input_line) as u16;
        assert_eq!(mouse::hit(cursor_x, cursor_y, &g), MouseTarget::Composer);
        app.resize(g.messages.width, g.messages.height);
        let mut terminal = Terminal::new(TestBackend::new(44, height)).unwrap();
        terminal
            .draw(|f| render::draw(f, &app, &Chrome { status: "" }))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(cursor_x, cursor_y)].bg,
            Color::White
        );
    }
}
