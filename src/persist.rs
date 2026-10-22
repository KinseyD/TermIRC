//! Persistent chat history in a local SQLite database.
//!
//! This module owns the storage seam: `core` and `history` stay std-only and
//! database-unaware; the application layer injects a [`HistorySink`] into the
//! `Session` and `main` assembles it from configuration. Every API takes an
//! injected `&Path` root (the `RetryPolicy`-style seam) so tests never touch
//! the user's real data directory.
//!
//! Failure handling follows the `logging::init` precedent: "run without
//! history rather than not run". Opening a corrupt or newer database, or an
//! append error after opening, disables the sink for the session with a single
//! `tracing::warn!` and is never fatal.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, params};

use crate::config::HistoryConfig;
use crate::core::{BufferKind, DeliveryState, Direction, MessageContent, MessageKind, ServerId};
use crate::protocol::parse_server_time;

/// Format version of the schema on disk, kept in `PRAGMA user_version`.
const SCHEMA_VERSION: u32 = 1;
/// Database file name inside the injected root directory.
const DB_FILE: &str = "history.sqlite";

/// Append-only SQLite-backed archive of chat messages and query renames.
///
/// The sink lives on the UI thread with the `Session`; all methods are
/// synchronous single-statement calls, which at human IRC rates is a page
/// cache operation (see the write-path rationale in
/// `docs/persistent-history.md`).
pub struct HistorySink {
    conn: Connection,
    exclude: HistoryConfig,
    /// Flipped to false after any database error: one warning, then the rest
    /// of the session runs without persistence (design: failure handling).
    enabled: bool,
}

impl HistorySink {
    /// Open (or create) the archive under `root` and apply retention.
    ///
    /// Returns `None` — after a single warning — when the directory cannot be
    /// created, the file cannot be opened, the database is corrupt, or its
    /// `user_version` is newer than ours (a future format; leave it
    /// untouched).
    pub fn open(root: &Path, config: &HistoryConfig) -> Option<HistorySink> {
        if let Err(error) = std::fs::create_dir_all(root) {
            tracing::warn!(
                "history persistence disabled: cannot create {}: {error}",
                root.display()
            );
            return None;
        }
        let path = root.join(DB_FILE);
        let conn = match Connection::open(&path) {
            Ok(conn) => conn,
            Err(error) => {
                tracing::warn!(
                    "history persistence disabled: cannot open {}: {error}",
                    path.display()
                );
                return None;
            }
        };
        // The archive is chat content: restrict it to the owning user where
        // the platform supports POSIX permissions (best-effort — SQLite may
        // have created the file with the process umask's default mode).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let sink = Self::init(conn, config);
        if let Some(sink) = &sink {
            sink.apply_retention(config.retention_days);
        }
        sink
    }

    fn init(conn: Connection, config: &HistoryConfig) -> Option<HistorySink> {
        let result = (|| -> rusqlite::Result<HistorySink> {
            // WAL keeps every-message commits cheap (no full fsync of the
            // main database per INSERT); NORMAL is the usual crash-safety
            // trade-off: a power loss may lose the last few transactions but
            // never corrupts the database — the same "lose seconds, not the
            // archive" contract the design gives the plain-text format.
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if version > SCHEMA_VERSION {
                // A newer format: leave the file untouched and run without
                // persistence rather than write records we cannot read back.
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    UnsupportedSchema(version),
                )));
            }
            conn.execute(SCHEMA_SQL, [])?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            Ok(HistorySink {
                conn,
                exclude: config.clone(),
                enabled: true,
            })
        })();
        match result {
            Ok(sink) => Some(sink),
            Err(error) => {
                tracing::warn!("history persistence disabled: {error}");
                None
            }
        }
    }

    /// Delete events older than `retention_days`, in one transaction.
    fn apply_retention(&self, retention_days: u64) {
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(retention_days.saturating_mul(86_400)))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let result = self.conn.execute(
            "DELETE FROM events WHERE kind = 'm' AND ts < ?",
            params![format_rfc3339(cutoff)],
        );
        if let Err(error) = result {
            tracing::warn!("history retention sweep failed: {error}");
        }
    }

    /// Persist one message. Only `Chat`/`Action` lines routed to a channel or
    /// query buffer are archived; console/system lines, delivery state, tags,
    /// and message IDs are session-scoped and never written.
    pub fn append(&mut self, server: &ServerId, target: &BufferKind, content: &MessageContent) {
        if !self.enabled {
            return;
        }
        let (target_kind, name) = match target {
            BufferKind::Channel(name) => ("c", name.as_str()),
            BufferKind::Query(name) => ("q", name.as_str()),
            // Server consoles are never persisted.
            BufferKind::Server => return,
        };
        if !matches!(content.kind, MessageKind::Chat | MessageKind::Action) {
            return;
        }
        if self.exclude.excludes(server.as_str(), name) {
            return;
        }
        let result = self.conn.execute(
            "INSERT INTO events (server, kind, target_kind, name, ts, dir, mkind, nick, text) \
             VALUES (?1, 'm', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                server.as_str(),
                target_kind,
                name,
                format_rfc3339(content.received_at),
                match content.direction {
                    Direction::Incoming => "i",
                    Direction::Outgoing => "o",
                },
                match content.kind {
                    MessageKind::Chat => "chat",
                    MessageKind::Action => "action",
                    _ => unreachable!("filtered above"),
                },
                content.nick,
                content.text,
            ],
        );
        if let Err(error) = result {
            // One warning, then silence for the rest of the session.
            tracing::warn!("history persistence disabled after append failure: {error}");
            self.enabled = false;
        }
    }

    /// Record a query rename on the clean path (`previous` had no collision).
    /// The collision path writes nothing: two separate conversations stay
    /// separate in the archive exactly as they do in memory.
    pub fn record_rename(&mut self, server: &ServerId, previous: &str, nickname: &str) {
        if !self.enabled {
            return;
        }
        let result = self.conn.execute(
            "INSERT INTO events (server, kind, name, new_name) VALUES (?1, 'n', ?2, ?3)",
            params![server.as_str(), previous, nickname],
        );
        if let Err(error) = result {
            tracing::warn!("history persistence disabled after rename failure: {error}");
            self.enabled = false;
        }
    }

    /// Load one server's archive, grouped by conversation.
    ///
    /// Records are grouped by literal `(target_kind, name)`; rename records
    /// then merge positionally: messages recorded before `N previous new`
    /// join the `new` conversation, messages recorded after a rename of
    /// `previous` stay in a separate `previous` conversation (a genuinely new
    /// one), and chains apply in record order.
    pub fn load(&self, server: &ServerId) -> Vec<(BufferKind, Vec<MessageContent>)> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT kind, target_kind, name, ts, dir, mkind, nick, text, new_name \
                      FROM events WHERE server = ?1 ORDER BY seq",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map(params![server.as_str()], |row| {
            Ok(RawEvent {
                kind: row.get(0)?,
                target_kind: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ts: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                dir: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                mkind: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                nick: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                text: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                new_name: row.get::<_, Option<String>>(8)?,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };

        // Query conversations keyed by literal name; channels cannot be
        // renamed and key on their own name directly.
        let mut queries: HashMap<String, Vec<MessageContent>> = HashMap::new();
        let mut channels: HashMap<String, Vec<MessageContent>> = HashMap::new();
        let mut order: Vec<BufferKind> = Vec::new();
        let noted = |kind: &BufferKind, order: &mut Vec<BufferKind>| {
            if !order.iter().any(|existing| existing == kind) {
                order.push(kind.clone());
            }
        };
        for row in rows.flatten() {
            match row.kind.as_str() {
                "n" => {
                    let Some(new_name) = row.new_name else {
                        continue;
                    };
                    // Positional merge: everything recorded under the old
                    // nickname so far precedes this rename and joins the new
                    // conversation. Messages recorded later under the old
                    // nickname start a fresh group below.
                    if let Some(previous_msgs) = queries.remove(&row.name) {
                        let entry = queries.entry(new_name.clone()).or_default();
                        if entry.is_empty() {
                            entry.extend(previous_msgs);
                        } else {
                            // The pre-rename records are chronologically
                            // ambiguous against literal `new` records; keep
                            // both, merged conversation first.
                            let mut merged = previous_msgs;
                            merged.append(entry);
                            *entry = merged;
                        }
                    }
                    if !order
                        .iter()
                        .any(|existing| matches!(existing, BufferKind::Query(n) if *n == new_name))
                    {
                        order.push(BufferKind::Query(new_name.clone()));
                    }
                }
                "m" => {
                    let Some(content) = restore(&row) else {
                        continue;
                    };
                    match row.target_kind.as_str() {
                        "c" => {
                            channels.entry(row.name.clone()).or_default().push(content);
                            noted(&BufferKind::Channel(row.name.clone()), &mut order);
                        }
                        "q" => {
                            queries.entry(row.name.clone()).or_default().push(content);
                            noted(&BufferKind::Query(row.name.clone()), &mut order);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        order
            .into_iter()
            .filter_map(|kind| {
                let name = match &kind {
                    BufferKind::Channel(name) | BufferKind::Query(name) => name.clone(),
                    BufferKind::Server => return None,
                };
                if self.exclude.excludes(server.as_str(), &name) {
                    return None;
                }
                let messages = match &kind {
                    BufferKind::Channel(_) => channels.remove(&name)?,
                    BufferKind::Query(_) => queries.remove(&name)?,
                    BufferKind::Server => return None,
                };
                Some((kind, messages))
            })
            .collect()
    }
}

/// A database whose `user_version` is newer than this build understands.
#[derive(Debug)]
struct UnsupportedSchema(u32);

impl std::fmt::Display for UnsupportedSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "history database has format version {} (this build understands {SCHEMA_VERSION}); \
             leaving it untouched",
            self.0
        )
    }
}

impl std::error::Error for UnsupportedSchema {}

struct RawEvent {
    kind: String,
    target_kind: String,
    name: String,
    ts: String,
    dir: String,
    mkind: String,
    nick: String,
    text: String,
    new_name: Option<String>,
}

/// Rebuild message content from a stored row: IDs are reminted by the
/// `HistoryStore`, delivery is `Received` (the archive only records messages
/// that were sent), and tags are wire metadata that does not outlive the line.
fn restore(row: &RawEvent) -> Option<MessageContent> {
    Some(MessageContent {
        nick: row.nick.clone(),
        text: row.text.clone(),
        kind: match row.mkind.as_str() {
            "chat" => MessageKind::Chat,
            "action" => MessageKind::Action,
            _ => return None,
        },
        direction: match row.dir.as_str() {
            "i" => Direction::Incoming,
            "o" => Direction::Outgoing,
            _ => return None,
        },
        received_at: parse_server_time(&row.ts)?,
        tags: Vec::new(),
        delivery: DeliveryState::Received,
    })
}

/// The single table holding every persistent event. `seq` is the shared
/// monotonic ordering key across messages and renames; renames order
/// relative to messages by `seq`, which is what the positional merge needs.
const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    server TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('m', 'n')),
    -- message columns (kind = 'm')
    target_kind TEXT CHECK (target_kind IN ('c', 'q')),
    name TEXT NOT NULL,
    ts TEXT,
    dir TEXT CHECK (dir IN ('i', 'o')),
    mkind TEXT CHECK (mkind IN ('chat', 'action')),
    nick TEXT,
    text TEXT,
    -- rename columns (kind = 'n')
    new_name TEXT,
    CHECK (
        (kind = 'm' AND target_kind IS NOT NULL AND ts IS NOT NULL
                      AND dir IS NOT NULL AND mkind IS NOT NULL
                      AND nick IS NOT NULL AND text IS NOT NULL
                      AND new_name IS NULL)
        OR (kind = 'n' AND new_name IS NOT NULL)
    )
)";

/// Format a UTC timestamp as `YYYY-MM-DDTHH:MM:SS.mmmZ`, the exact shape
/// `parse_server_time` accepts, using the civil-from-days inverse of the
/// parser's Howard Hinnant days algorithm.
fn format_rfc3339(time: SystemTime) -> String {
    let duration = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = duration.as_secs() as i64;
    let millis = duration.subsec_millis();
    let days = secs.div_euclid(86_400);
    let seconds_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        month,
        day,
        seconds_of_day / 3_600,
        (seconds_of_day / 60) % 60,
        seconds_of_day % 60,
        millis
    )
}

/// Convert days since the Unix epoch to a proleptic Gregorian date
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::MessageContent;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("termirc-persist-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn history_config(exclude: &[&str]) -> HistoryConfig {
        HistoryConfig {
            persist: true,
            retention_days: 7,
            exclude: exclude.iter().map(|entry| entry.to_string()).collect(),
        }
    }

    fn sink(root: &Path) -> HistorySink {
        HistorySink::open(root, &history_config(&[])).expect("sink opens on a fresh root")
    }

    fn server() -> ServerId {
        ServerId::new("libera")
    }

    fn chat(nick: &str, text: &str) -> MessageContent {
        MessageContent::chat(nick, text)
    }

    #[test]
    fn round_trips_tabs_newlines_backslash_and_cjk_through_sqlite() {
        // Arrange
        let root = temp_root("roundtrip");
        let mut sink = sink(&root);
        let text = "a\tb\nc\\d \"quoted\" 'single' 你好日本語 \u{0}？";
        let mut content = chat("nick", text);
        content.received_at = SystemTime::UNIX_EPOCH + Duration::new(1_790_899_200, 0);

        // Act
        sink.append(&server(), &BufferKind::Channel("#a".into()), &content);
        let loaded = sink.load(&server());

        // Assert
        assert_eq!(loaded.len(), 1);
        let (kind, messages) = &loaded[0];
        assert_eq!(kind, &BufferKind::Channel("#a".into()));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, text);
        assert_eq!(messages[0].nick, "nick");
        assert_eq!(messages[0].received_at, content.received_at);
        assert_eq!(messages[0].delivery, DeliveryState::Received);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn round_trips_every_kind_direction_and_target_variant() {
        // Arrange
        let root = temp_root("variants");
        let mut sink = sink(&root);
        let server = server();
        let mut action = chat("me", "waves");
        action.kind = MessageKind::Action;
        action.direction = Direction::Outgoing;
        let mut outgoing = chat("me", "hello");
        outgoing.direction = Direction::Outgoing;

        // Act
        sink.append(&server, &BufferKind::Channel("#a".into()), &chat("a", "in"));
        sink.append(&server, &BufferKind::Channel("#a".into()), &action);
        sink.append(&server, &BufferKind::Query("alice".into()), &outgoing);
        let loaded = sink.load(&server);

        // Assert
        assert_eq!(loaded.len(), 2);
        let channel = loaded
            .iter()
            .find(|(kind, _)| matches!(kind, BufferKind::Channel(_)))
            .unwrap();
        assert_eq!(channel.1[0].kind, MessageKind::Chat);
        assert_eq!(channel.1[0].direction, Direction::Incoming);
        assert_eq!(channel.1[1].kind, MessageKind::Action);
        assert_eq!(channel.1[1].direction, Direction::Outgoing);
        let query = loaded
            .iter()
            .find(|(kind, _)| matches!(kind, BufferKind::Query(_)))
            .unwrap();
        assert_eq!(query.1[0].direction, Direction::Outgoing);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn console_and_system_lines_and_server_buffers_are_never_persisted() {
        // Arrange
        let root = temp_root("nonchat");
        let mut sink = sink(&root);
        let server = server();

        // Act
        sink.append(&server, &BufferKind::Server, &chat("a", "x"));
        sink.append(
            &server,
            &BufferKind::Channel("#a".into()),
            &MessageContent::console("note"),
        );
        sink.append(
            &server,
            &BufferKind::Channel("#a".into()),
            &MessageContent::system("note"),
        );

        // Assert
        assert!(sink.load(&server).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_merges_pre_rename_records_and_splits_post_rename_reuse() {
        // Arrange
        let root = temp_root("rename-split");
        let mut sink = sink(&root);
        let server = server();
        let query = |nick: &str| BufferKind::Query(nick.into());

        // Act
        sink.append(&server, &query("alice"), &chat("alice", "before"));
        sink.record_rename(&server, "alice", "alicia");
        sink.append(&server, &query("alice"), &chat("alice", "after reuse"));
        sink.append(&server, &query("alicia"), &chat("alicia", "new convo"));
        let loaded = sink.load(&server);

        // Assert
        let get = |nick: &str| {
            loaded
                .iter()
                .find(|(kind, _)| matches!(kind, BufferKind::Query(n) if n == nick))
                .map(|(_, messages)| messages)
                .expect("conversation present")
        };
        assert_eq!(
            get("alicia")
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["before", "new convo"]
        );
        assert_eq!(
            get("alice")
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["after reuse"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_chains_apply_in_record_order() {
        // Arrange
        let root = temp_root("rename-chain");
        let mut sink = sink(&root);
        let server = server();
        let query = |nick: &str| BufferKind::Query(nick.into());

        // Act
        sink.append(&server, &query("alice"), &chat("alice", "one"));
        sink.record_rename(&server, "alice", "alicia");
        sink.append(&server, &query("alicia"), &chat("alicia", "two"));
        sink.record_rename(&server, "alicia", "alice2");
        sink.append(&server, &query("alice2"), &chat("alice2", "three"));
        let loaded = sink.load(&server);

        // Assert
        assert_eq!(loaded.len(), 1);
        let (kind, messages) = &loaded[0];
        assert_eq!(kind, &BufferKind::Query("alice2".into()));
        assert_eq!(
            messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["one", "two", "three"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collision_path_records_nothing_so_both_conversations_stay_separate() {
        // Arrange: the sink API has no collision-path write; simulate the
        // session's collision behavior by writing both conversations without
        // any rename record and verifying load keeps them apart.
        let root = temp_root("collision");
        let mut sink = sink(&root);
        let server = server();
        let query = |nick: &str| BufferKind::Query(nick.into());

        // Act
        sink.append(&server, &query("alice"), &chat("alice", "old convo"));
        sink.append(&server, &query("alicia"), &chat("alicia", "existing convo"));

        // Assert
        let loaded = sink.load(&server);
        assert_eq!(loaded.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn retention_deletes_only_events_older_than_the_cutoff() {
        // Arrange
        let root = temp_root("retention");
        let config = HistoryConfig {
            persist: true,
            retention_days: 7,
            exclude: Vec::new(),
        };
        let mut sink = HistorySink::open(&root, &config).unwrap();
        let server = server();
        let now = SystemTime::now();
        let mut old = chat("a", "old");
        old.received_at = now - Duration::from_secs(8 * 86_400);
        let mut fresh = chat("a", "fresh");
        fresh.received_at = now - Duration::from_secs(60);

        // Act
        sink.append(&server, &BufferKind::Channel("#a".into()), &old);
        sink.append(&server, &BufferKind::Channel("#a".into()), &fresh);
        let reopened = HistorySink::open(&root, &config).expect("reopen applies retention");
        let loaded = reopened.load(&server);

        // Assert
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0]
                .1
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["fresh"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exclude_is_honored_on_both_write_and_read() {
        // Arrange
        let root = temp_root("exclude");
        let mut sink = HistorySink::open(&root, &history_config(&["#huge", "srv/alice"])).unwrap();
        let server = ServerId::new("srv");
        let other = ServerId::new("other");
        let channel = BufferKind::Channel("#huge".into());
        let query = BufferKind::Query("alice".into());
        let kept = BufferKind::Query("alice".into());

        // Act
        sink.append(&server, &channel, &chat("a", "excluded everywhere"));
        sink.append(&server, &query, &chat("a", "excluded for srv"));
        sink.append(&other, &query, &chat("a", "kept for other server"));
        sink.append(&server, &kept, &chat("a", "never written"));

        // Assert
        assert!(sink.load(&server).is_empty());
        let loaded = sink.load(&other);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].1.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_database_disables_persistence_for_the_session_without_touching_the_file() {
        // Arrange: a file that is not a SQLite database at all.
        let root = temp_root("corrupt");
        std::fs::create_dir_all(&root).unwrap();
        let before = b"definitely not a database".to_vec();
        std::fs::write(root.join(DB_FILE), &before).unwrap();

        // Act
        let sink = HistorySink::open(&root, &history_config(&[]));

        // Assert
        assert!(sink.is_none());
        assert_eq!(std::fs::read(root.join(DB_FILE)).unwrap(), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn newer_user_version_is_left_untouched_and_disables_the_sink() {
        // Arrange: a valid database stamped with a future format version.
        let root = temp_root("future");
        std::fs::create_dir_all(&root).unwrap();
        let conn = Connection::open(root.join(DB_FILE)).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);

        // Act
        let sink = HistorySink::open(&root, &history_config(&[]));

        // Assert
        assert!(sink.is_none());
        let conn = Connection::open(root.join(DB_FILE)).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 99);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_groups_servers_independently() {
        // Arrange
        let root = temp_root("servers");
        let mut sink = sink(&root);
        let one = ServerId::new("one");
        let two = ServerId::new("two");

        // Act
        sink.append(&one, &BufferKind::Channel("#a".into()), &chat("a", "one's"));
        sink.append(&two, &BufferKind::Channel("#a".into()), &chat("a", "two's"));

        // Assert
        assert_eq!(sink.load(&one)[0].1[0].text, "one's");
        assert_eq!(sink.load(&two)[0].1[0].text, "two's");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn format_rfc3339_round_trips_through_parse_server_time() {
        for secs in [0i64, 1_790_899_200, 4_102_444_800, 951_782_400] {
            let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64);
            let formatted = format_rfc3339(time);
            assert_eq!(
                parse_server_time(&formatted),
                Some(time),
                "{formatted} must round-trip"
            );
        }
    }
}
