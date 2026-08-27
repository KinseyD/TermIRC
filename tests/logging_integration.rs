//! Logging integration: install the real subscriber into a temp dir and
//! verify events reach the daily file. One test only — the global
//! subscriber can be installed once per process.

#[test]
fn init_writes_events_to_the_daily_log_file() {
    // Arrange: a fresh temp dir (unique per test run).
    let dir = std::env::temp_dir().join(format!("termirc-logtest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Act: the blocking appender writes synchronously - no flush needed.
    termirc::logging::init(&dir).unwrap();
    tracing::info!("termirc probe 260824");

    // Assert: exactly one log file named termirc.log.YYYY-MM-DD exists and
    // carries the event text.
    let files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    assert_eq!(files.len(), 1, "files: {files:?}");
    let name = files[0]
        .file_name()
        .unwrap()
        .to_str()
        .expect("log file name is valid UTF-8");
    let date = name
        .strip_prefix("termirc.log.")
        .unwrap_or_else(|| panic!("log file name is not termirc.log.*: {name}"));
    let bytes = date.as_bytes();
    assert_eq!(bytes.len(), 10, "date suffix is not YYYY-MM-DD: {date}");
    assert!(
        bytes[0..4].iter().all(u8::is_ascii_digit)
            && bytes[4] == b'-'
            && bytes[5..7].iter().all(u8::is_ascii_digit)
            && bytes[7] == b'-'
            && bytes[8..].iter().all(u8::is_ascii_digit),
        "date suffix is not YYYY-MM-DD: {date}"
    );
    let content = std::fs::read_to_string(&files[0]).unwrap();
    assert!(
        content.contains("termirc probe 260824"),
        "log content: {content}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
