//! Runtime logging: daily-rotated files under the config directory.

use std::path::Path;

use tracing_appender::rolling::{RollingFileAppender, Rotation};

/// How many rotated log files to keep on disk.
const MAX_LOG_FILES: usize = 7;
/// Log file name prefix (no trailing dot: tracing-appender joins prefix and
/// date with `.`); daily rotation produces `termirc.log.2026-08-24`.
const FILE_PREFIX: &str = "termirc.log";

/// Build the daily-rolling appender for `dir` (created if missing).
///
/// Split out from [`init`] so the failure path is unit-testable without
/// touching the process-global subscriber.
pub fn build_appender(dir: &Path) -> anyhow::Result<RollingFileAppender> {
    std::fs::create_dir_all(dir)
        .map_err(|e| anyhow::anyhow!("failed to create log dir {}: {e}", dir.display()))?;
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(dir)
        .map_err(|e| anyhow::anyhow!("failed to open log file in {}: {e}", dir.display()))
}

/// Install the global tracing subscriber writing to `dir`.
///
/// Call once, as early as possible in `main`. Failure is NOT fatal for the
/// caller: return `Err` and keep running without logs.
pub fn init(dir: &Path) -> anyhow::Result<()> {
    let appender = build_appender(dir)?;
    tracing_subscriber::fmt()
        .with_writer(appender)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to install the global log subscriber: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_appender_creates_missing_directory() {
        // Arrange: a nested dir that does not exist yet.
        let base = std::env::temp_dir().join(format!("termirc-log-mk-{}", std::process::id()));
        let dir = base.join("nested").join("logs");
        let _ = std::fs::remove_dir_all(&base);

        // Act
        let appender = build_appender(&dir);
        drop(appender);

        // Assert
        assert!(dir.is_dir(), "log dir was not created");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn build_appender_fails_when_a_file_blocks_the_path() {
        // Arrange: a regular file where a directory component must go.
        let base = std::env::temp_dir().join(format!("termirc-log-blk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let blocker = base.join("blocker");
        std::fs::write(&blocker, b"").unwrap();

        // Act
        let result = build_appender(&blocker.join("logs"));

        // Assert
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
