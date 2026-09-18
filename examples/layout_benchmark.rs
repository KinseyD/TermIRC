//! Local, deterministic workload; no configuration or network access.
//! Run with: cargo run --release --locked --offline --example layout_benchmark
use std::{hint::black_box, time::Instant};
use termirc::{core::RoutedMessage, tui::App};

fn main() {
    let mut app = App::new(80, 20);
    app.open_channel("benchmark", "#history");
    let body = "x".repeat(100);
    for _ in 0..5_000 {
        app.push_message(RoutedMessage::chat("benchmark", "#history", "n", &body));
    }
    app.select_buffer(0);
    let ids: Vec<_> = app.messages().iter().map(|message| message.id).collect();
    let started = Instant::now();
    app.resize(10, 20);
    let resize_elapsed = started.elapsed();
    assert_eq!(
        app.messages()
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        ids
    );
    let started = Instant::now();
    black_box(app.visible_lines());
    let render_elapsed = started.elapsed();
    println!("5000 messages × 100 bytes; width 80 → 10; viewport 20 rows");
    println!("resize: {resize_elapsed:?}; visible rows: {render_elapsed:?}");
    println!(
        "retained: {}; layout rows: {}; IDs unchanged",
        app.messages().len(),
        app.total_height()
    );
}
