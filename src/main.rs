//! Startup, configured resources and terminal restoration.
use anyhow::Context;
use clap::Parser;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};

use termirc::cli::{Cli, Command};
use termirc::config::Config;
use termirc::connection::{ConnectionHandle, IrcEvent, spawn_irc};
use termirc::send::SendFailure;
use termirc::tui::App;
fn main() -> anyhow::Result<()> {
    install_panic_hook();

    let home = dirs::home_dir().context(termirc::i18n::t("err.startup_home"))?;
    // CLI parsing precedes everything else: --help/--version/parse errors
    // exit before logging or the terminal is touched. An explicit
    // --config PATH overrides the hardcoded default; logs always stay in
    // the default directory so alternate profiles do not scatter them.
    let cli = Cli::parse();
    let config_path = match cli.config {
        Some(path) => path,
        None => home.join(".config").join("termirc").join("config.toml"),
    };
    let logs_dir = home.join(".config").join("termirc").join("logs");
    // Logging failures are not fatal: run without logs rather than not run.
    // Deliberately not localized: logging starts before the configuration
    // exists, so the language preference is unknowable here.
    if let Err(e) = termirc::logging::init(&logs_dir) {
        eprintln!("logging disabled: {e:#}");
    }
    // The locale initializes from the same file that is about to load, so
    // config validation errors render in the configured language whenever
    // the file can be read at all (and English otherwise — the catalog's
    // built-in fallback). The catalog falls back to the embedded defaults
    // for any language whose file is absent or incomplete, warning once per
    // problem, so a broken locale can never keep the client from starting.
    let ui = termirc::config::load_ui(&config_path);
    termirc::i18n::init(&ui.language);
    // A missing config file means first run: start with an empty config
    // and let the TUI's setup wizard create one. A file that exists but
    // cannot be read or parsed is still a hard error — silently
    // replacing a broken config the user wrote would destroy work.
    let config = if config_path.exists() {
        match Config::load(&config_path) {
            Ok(config) => config,
            Err(e) => {
                // Deliberately not localized either: a failure here
                // means the config file could not be read or parsed, in
                // which case the language selection inside it is
                // unknowable.
                let e = e.context(termirc::i18n::format(
                    termirc::i18n::t("err.startup_config_load"),
                    &[("path", &config_path.display().to_string())],
                ));
                // Log the top-level context only: the toml error chain
                // embeds the raw offending source line, which could
                // persist a password fragment into the log file.
                tracing::error!("{e}");
                return Err(e);
            }
        }
    } else {
        Config::default()
    };
    // `--listen` and `send` are both headless but contradictory: listen
    // mode promises that nothing is ever sent. clap cannot refuse the pair
    // itself (`--config` has to stay valid beside the subcommand), so it is
    // refused here — before listen mode could start, and without connecting.
    if let Some(Command::Send { .. }) = cli.command
        && let Some(failure) = termirc::send::listen_conflict(cli.listen)
            .or_else(|| termirc::send::stdio_conflict(cli.stdio))
    {
        fail_send(&failure);
    }
    // `--listen` is headless and read-only: branch off before any
    // terminal takeover so the TUI path below stays exactly as it was.
    if cli.listen {
        // No setup wizard exists without a terminal, and a missing
        // config file defaults to zero servers — waiting forever with
        // nothing to print would look like a hang, so fail fast.
        if config.first_server().is_none() {
            let error = anyhow::anyhow!(termirc::i18n::t("listen.no_servers"));
            tracing::error!("{error}");
            return Err(error);
        }
        tracing::info!(
            "termirc v{} starting in listen mode; config={}; servers={}",
            env!("CARGO_PKG_VERSION"),
            config_path.display(),
            config.servers.len()
        );
        return run_listen(&config);
    }
    // `--stdio` is headless and bidirectional: the same workers `--listen`
    // spawns, plus one queued send per stdin line. It branches off before
    // any terminal takeover, exactly like the other headless modes.
    if cli.stdio {
        // No setup wizard exists without a terminal, and a missing config
        // file defaults to zero servers — waiting forever with nothing to
        // answer would look like a hang, so fail fast.
        if config.first_server().is_none() {
            let error = anyhow::anyhow!(termirc::i18n::t("listen.no_servers"));
            tracing::error!("{error}");
            return Err(error);
        }
        tracing::info!(
            "termirc v{} starting in stdio mode; config={}; servers={}",
            env!("CARGO_PKG_VERSION"),
            config_path.display(),
            config.servers.len()
        );
        return run_stdio(&config);
    }
    // `send` is headless too: one message to one server, then a scriptable
    // exit code. Like listen mode it branches off before any terminal
    // takeover, and it never returns.
    if let Some(Command::Send { buffer, message }) = cli.command {
        run_send(&config, &config_path, &buffer, &message);
    }
    if config.first_server().is_none() {
        // Not fatal: the setup wizard opens on the first frame and can
        // create the missing entry (or the user quits and writes the
        // file by hand).
        tracing::warn!("config has no servers; opening the setup wizard");
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
    // Bracketed paste is a safety boundary: pasted newlines arrive as one
    // paste event instead of Enter keys, so they cannot auto-submit a draft
    // or execute pasted slash commands line by line.
    if ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)
        .is_err()
    {
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture
        );
        ratatui::restore();
        anyhow::bail!("{}", termirc::i18n::t("err.startup_mouse"));
    }
    let mut config = config;
    let result = run(&mut terminal, &mut config, &config_path);
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
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
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture
        );
        let _ = ratatui::try_restore();
        default_hook(info);
    }));
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    config: &mut Config,
    config_path: &std::path::Path,
) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    // The handle map doubles as the outgoing-message routing table and
    // must outlive the event loop, so the setup wizard can spawn more
    // servers later the exact same way.
    let mut outgoing = spawn_workers(config, &tx);

    let app = assemble_session(config);
    termirc::tui::events::run(terminal, config, app, &mut outgoing, tx, rx, config_path)
}

/// Spawn one IRC worker per configured server onto `tx`, each joining
/// all of that server's channels, and return each connection's handle
/// keyed by server name.
///
/// Threads are deliberately not joined: they block on network I/O and
/// are reaped when the process exits. Dropping a handle cancels pending
/// attempts and closes the active socket, so callers must keep the map
/// alive for as long as the workers should run.
fn spawn_workers(
    config: &Config,
    tx: &std::sync::mpsc::Sender<IrcEvent>,
) -> std::collections::HashMap<String, ConnectionHandle> {
    let mut workers = std::collections::HashMap::new();
    for (name, server) in config.servers.iter() {
        let (_thread, handle) = spawn_irc(
            server.clone(),
            name.clone(),
            server.channels.clone(),
            tx.clone(),
        );
        workers.insert(name.clone(), handle);
    }
    workers
}

/// Headless read-only mode: spawn the same workers the TUI would, keep
/// their handles alive for the loop, and print incoming lines until
/// every worker has exited. Nothing here ever sends; Ctrl+C terminates.
fn run_listen(config: &Config) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let workers = spawn_workers(config, &tx);
    // Drop the last original sender so `recv` can observe "every worker
    // exited" instead of being kept alive by this clone forever.
    drop(tx);
    println!(
        "{}",
        termirc::i18n::format(
            termirc::i18n::t("listen.started"),
            &[("count", &workers.len().to_string())]
        )
    );
    termirc::listen::run(workers, rx)
}

/// Headless stdio mode: spawn the same workers the TUI would, queue one
/// send per stdin line against their readiness, and print every rendered
/// event until stdin closes and the verdict drain finishes.
fn run_stdio(config: &Config) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let workers = spawn_workers(config, &tx);
    // Drop the last original sender so `recv` can observe "every worker
    // exited" instead of being kept alive by this clone forever.
    drop(tx);
    let exit = termirc::stdio::run(workers, rx)?;
    // A refusal after stdin closed means piped input was dropped: the
    // exit code tells the driving script (like send mode, without the
    // failure text, which stderr already carries).
    if exit.code() != 0 {
        std::process::exit(exit.code());
    }
    Ok(())
}

/// Headless one-shot send: read the message from stdin when it is `-`,
/// deliver it, and exit with the code scripts branch on. Nothing is printed
/// on success, and neither the log nor stderr ever carries message text.
fn run_send(config: &Config, config_path: &std::path::Path, buffer: &str, message: &str) -> ! {
    // The buffer address names the server and the target; where the text
    // comes from is all the log ever learns about it.
    tracing::info!(
        "termirc v{} starting in send mode; config={}; buffer={}; message={}",
        env!("CARGO_PKG_VERSION"),
        config_path.display(),
        buffer,
        if message == "-" { "stdin" } else { "argument" }
    );
    let outcome = match message {
        "-" => match read_stdin_message() {
            Ok(message) => termirc::send::run(config, buffer, &message),
            Err(error) => {
                tracing::error!("send mode could not read stdin: {error}");
                Err(SendFailure::StdinUnreadable)
            }
        },
        message => termirc::send::run(config, buffer, message),
    };
    if let Err(failure) = &outcome {
        fail_send(failure);
    }
    tracing::info!("send mode delivered to {buffer}; exiting");
    std::process::exit(0);
}

/// Report a `send` that will not happen and exit with its class's code: the
/// one way send mode leaves the process without delivering.
fn fail_send(failure: &SendFailure) -> ! {
    tracing::error!(
        "send mode failed; exiting with {}: {failure}",
        failure.exit_code()
    );
    eprintln!("termirc send: {failure}");
    std::process::exit(failure.exit_code())
}

/// The whole of stdin as the message, minus the one newline a shell pipes
/// along (`\n` or `\r\n`). Any other newline stays and is refused by the
/// normal validation, exactly like one typed into the composer.
fn read_stdin_message() -> std::io::Result<String> {
    let mut message = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut message)?;
    Ok(strip_trailing_newline(&message).to_owned())
}

fn strip_trailing_newline(message: &str) -> &str {
    match message.strip_suffix('\n') {
        Some(body) => body.strip_suffix('\r').unwrap_or(body),
        None => message,
    }
}

/// Build the application session exactly as startup does, without spawning
/// workers: `run()` assembles it so tests can pin the startup invariants.
fn assemble_session(config: &Config) -> App {
    let mut app = App::new(1, 1);
    // Persistence is opt-in: assemble the sink from configuration the same
    // way logging is, and run without history rather than not run.
    if config.history.persist
        && let Some(root) = dirs::data_dir().map(|dir| dir.join("termirc"))
        && let Some(sink) = termirc::persist::HistorySink::open(&root, &config.history)
    {
        app.session = termirc::application::Session::with_persist(5_000, sink);
    }
    // Buffers for every configured server (plus its channels and queries)
    // must exist before the first worker event arrives: the per-server
    // console is where the welcome burst (001..004, MOTD) lands and where
    // raw commands are composed. Registering after the optional
    // `Session::with_persist` swap keeps the buffers on the live session.
    app.session.register_config(config);
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembled_session_opens_a_console_buffer_for_every_configured_server() {
        // Arrange: two servers, TOML-free via the same parse entry point the
        // config tests use, so this pins `register_config` in startup itself.
        let config = Config::parse(
            r##"
[servers.alpha]
username="me"
nickname="me"
password=""
server="localhost"
port=6667
channels=[]

[servers.beta]
username="me"
nickname="me"
password=""
server="localhost"
port=6667
channels=[]
"##,
        )
        .unwrap();

        // Act
        let mut app = assemble_session(&config);

        // Assert: one `BufferKind::Server` console per configured server id,
        // matching what `run()` used to set up inline before the first
        // worker event arrives.
        assert_eq!(
            app.session.buffer_count(),
            config.servers.len(),
            "each configured server gets exactly one buffer"
        );
        for name in ["alpha", "beta"] {
            let found = (0..app.session.buffer_count()).any(|index| {
                app.session.select_buffer(index);
                app.session.active_buffer().is_some_and(|buffer| {
                    buffer.server == termirc::core::ServerId::new(name)
                        && buffer.kind == termirc::core::BufferKind::Server
                })
            });
            assert!(found, "no console buffer registered for {name}");
        }
    }

    #[test]
    fn a_first_run_session_boots_without_servers_or_buffers() {
        // Arrange: no config file existed, so startup uses the default.
        let config = Config::default();

        // Act
        let app = assemble_session(&config);

        // Assert: no buffers, nothing viewed, and the app is still
        // running — the welcome page plus the setup wizard is a valid
        // first frame (startup no longer bails on zero servers).
        assert!(config.first_server().is_none());
        assert_eq!(app.session.buffer_count(), 0);
        assert!(app.active_buffer().is_none());
        assert!(app.is_running());
    }

    #[test]
    fn stdin_loses_exactly_the_one_trailing_newline() {
        for (input, expected) in [
            ("hello world\n", "hello world"),
            ("hello world\r\n", "hello world"),
            // Only one: a second newline is message content, refused as such.
            ("hello world\n\n", "hello world\n"),
            // A lone carriage return is not a line ending; validation
            // rejects it like any other control character.
            ("hello world\r", "hello world\r"),
            ("hello world", "hello world"),
            ("\n", ""),
        ] {
            assert_eq!(strip_trailing_newline(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn listen_and_send_parse_together_and_send_refuses_the_pair() {
        // Arrange: clap accepts the pair — it has to accept `--config`
        // beside the subcommand — so nothing upstream rejects it.
        let both = Cli::try_parse_from(["termirc", "--listen", "send", "srv/#c", "hi"])
            .expect("--listen beside send parses");

        // Act
        let refusal = termirc::send::listen_conflict(both.listen);

        // Assert: a local rejection (exit 2), while `send` alone is
        // unaffected.
        assert_eq!(refusal, Some(SendFailure::ListenConflict));
        assert_eq!(refusal.unwrap().exit_code(), 2);
        let alone = Cli::try_parse_from(["termirc", "send", "srv/#c", "hi"]).unwrap();
        assert_eq!(termirc::send::listen_conflict(alone.listen), None);
    }

    #[test]
    fn an_unreadable_stdin_is_a_local_rejection() {
        let failure = SendFailure::StdinUnreadable;
        assert_eq!(failure.exit_code(), 2);
        assert_eq!(failure.to_string(), "could not read the message from stdin");
    }
}
