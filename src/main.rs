//! Startup, configured resources and terminal restoration.
use anyhow::Context;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use termirc::config::Config;
use termirc::connection::{ConnectionHandle, spawn_irc};
use termirc::tui::App;
fn main() -> anyhow::Result<()> {
    install_panic_hook();

    let home = dirs::home_dir().context(termirc::i18n::t("err.startup_home"))?;
    let config_path = home.join(".config").join("termirc").join("config.toml");
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
    let config = match Config::load(&config_path) {
        Ok(config) => config,
        Err(e) => {
            // Deliberately not localized either: a failure here means the
            // config file could not be read or parsed, in which case the
            // language selection inside it is unknowable.
            let e = e.context(termirc::i18n::format(
                termirc::i18n::t("err.startup_config_load"),
                &[("path", &config_path.display().to_string())],
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
        anyhow::bail!("{}", termirc::i18n::t("err.startup_no_servers"));
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
        anyhow::bail!("{}", termirc::i18n::t("err.startup_mouse"));
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
    let mut outgoing: std::collections::HashMap<String, ConnectionHandle> =
        std::collections::HashMap::new();
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

    let mut app = App::new(1, 1);
    // Persistence is opt-in: assemble the sink from configuration the same
    // way logging is, and run without history rather than not run.
    if config.history.persist
        && let Some(root) = dirs::data_dir().map(|dir| dir.join("termirc"))
        && let Some(sink) = termirc::persist::HistorySink::open(&root, &config.history)
    {
        app.session = termirc::application::Session::with_persist(5_000, sink);
    }
    termirc::tui::events::run(terminal, config, app, outgoing, rx)
}
