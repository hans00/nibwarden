//! `nibwardend`: the per-user daemon that holds the vault (phase1.md §5).

// Windows: no console window when started at login or from a window. Its log is seen by
// redirecting stderr (`nibwardend 2> log`); debug builds keep the console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod approvals;
mod browser;
mod control;
mod daemon;
mod error;
mod item_data;
mod prompt_broker;
mod push_sync;
mod send_data;
mod ssh_agent;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use clap::Parser;
use nibwarden_core::Store;
use nibwarden_ipc::LockState;
use nibwarden_platform::{
    hardening::harden_process,
    paths,
    session_events::{self, SessionEvent},
    signal,
    transport::Listener,
};

use crate::daemon::{Config, Daemon};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Where the session and encrypted vault cache live
    /// [default: $XDG_DATA_HOME/nibwarden].
    #[arg(long, env = "NIBWARDEN_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Control socket [default: $XDG_RUNTIME_DIR/nibwarden/daemon.sock].
    #[arg(long, env = "NIBWARDEN_SOCKET")]
    socket: Option<PathBuf>,
    /// SSH agent socket [default: ssh-agent.sock next to the control socket].
    #[arg(long, env = "NIBWARDEN_SSH_AGENT_SOCKET")]
    ssh_agent_socket: Option<PathBuf>,
    /// Do not serve the SSH agent.
    #[arg(long, conflicts_with = "ssh_agent_socket")]
    no_ssh_agent: bool,
    /// The `nibwarden-gui` binary for unlock windows [default: next to nibwardend].
    #[arg(long, env = "NIBWARDEN_GUI")]
    gui: Option<PathBuf>,
    /// Lock after this many seconds without vault use; 0 = never [default: the settings
    /// file's `[lock] idle_minutes`, 15 minutes].
    #[arg(long)]
    lock_timeout: Option<u64>,
    /// Sync every this many seconds while logged in; 0 = only on demand.
    #[arg(long, default_value_t = 300)]
    sync_interval: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    harden_process();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,nibwardend=info".into()),
        )
        .with_writer(std::io::stderr)
        // Colours only on a terminal: under systemd, stderr goes to the journal.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();

    let args = Args::parse();
    let data_dir = match args.data_dir {
        Some(dir) => dir,
        None => paths::data_dir().context("no data directory; pass --data-dir")?,
    };
    let socket = match args.socket {
        Some(path) => path,
        None => nibwarden_ipc::default_socket_path().context(
            "no runtime directory for the socket (XDG_RUNTIME_DIR is unset); pass --socket",
        )?,
    };
    let agent_socket = match (args.no_ssh_agent, args.ssh_agent_socket) {
        (true, _) => None,
        (false, Some(path)) => Some(path),
        (false, None) => Some(socket.with_file_name("ssh-agent.sock")),
    };
    let seconds = |s: u64| (s > 0).then(|| Duration::from_secs(s));
    let config = Config {
        lock_timeout: args.lock_timeout.map(seconds),
        sync_interval: seconds(args.sync_interval),
        ssh_agent_socket: agent_socket.clone(),
        gui: args.gui.or_else(|| {
            let sibling = std::env::current_exe()
                .ok()?
                .with_file_name(format!("nibwarden-gui{}", std::env::consts::EXE_SUFFIX));
            sibling.exists().then_some(sibling)
        }),
    };

    // Bind first: a second daemon must fail before it touches the shared database.
    let listener = Listener::bind(&socket)
        .await
        .with_context(|| format!("cannot listen on {}", socket.display()))?;
    let agent_listener = match &agent_socket {
        Some(path) => Some(
            Listener::bind(path)
                .await
                .with_context(|| format!("cannot listen on {}", path.display()))?,
        ),
        None => None,
    };
    let store = Store::open(&data_dir)?;
    let daemon = Daemon::start(store, config, nibwarden_config::load()).await?;
    tracing::info!(socket = %socket.display(), data_dir = %data_dir.display(), "listening");
    if let Some(path) = &agent_socket {
        tracing::info!(socket = %path.display(), "ssh agent listening");
    }
    // The browsers may name the browser host of an older installation (a Nix store path
    // changes with every update).
    if let Ok(exe) = std::env::current_exe() {
        let host = exe.with_file_name(format!(
            "nibwarden-browser-host{}",
            std::env::consts::EXE_SUFFIX
        ));
        if host.exists()
            && let Err(e) = nibwarden_platform::native_messaging::refresh(&host)
        {
            tracing::warn!(error = %e, "cannot update the browser extension's host");
        }
    }
    let agent = {
        let daemon = daemon.clone();
        async move {
            match agent_listener {
                Some(listener) => ssh_agent::serve(daemon, listener).await,
                None => std::future::pending().await,
            }
        }
    };

    // Dropping the listeners at the end of the select removes their sockets.
    tokio::select! {
        () = control::serve(daemon.clone(), listener) => {}
        () = agent => {}
        () = daemon.clone().run_timers() => {}
        () = lock_on_session_events(daemon.clone()) => {}
        () = push_sync::run(daemon.clone()) => {}
        result = signal::shutdown_requested() => result?,
    }

    tracing::info!("shutting down");
    daemon.lock().await.ok();
    Ok(())
}

/// Locks the vault when the screen locks or the machine is about to sleep (phase2.md
/// §4.3), as configured. Never returns.
async fn lock_on_session_events(daemon: Arc<Daemon>) {
    {
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            if let Err(e) = session_events::watch(tx).await {
                tracing::info!(error = %e, "session events unavailable; no lock on screen lock or sleep");
            }
        });
        while let Some(event) = events.recv().await {
            let lock = daemon.lock_settings();
            let wanted = match event {
                SessionEvent::ScreenLocked => lock.on_screen_lock,
                SessionEvent::Sleeping => lock.on_sleep,
            };
            if wanted && daemon.lock_state().await == LockState::Unlocked {
                tracing::info!(?event, "locking");
                if let Err(e) = daemon.lock().await {
                    tracing::warn!(error = %e, "lock failed");
                }
            }
        }
    }
    std::future::pending().await
}

#[cfg(test)]
mod tests {
    /// Every option is in the man page (packaging/man/nibwardend.1).
    #[test]
    fn man_page_covers_the_options() {
        let page = include_str!("../../../packaging/man/nibwardend.1").replace("\\-", "-");
        let command = <super::Args as clap::CommandFactory>::command();
        let missing: Vec<&str> = command
            .get_arguments()
            .filter_map(|a| a.get_long())
            .filter(|long| !["help", "version"].contains(long))
            .filter(|long| !page.contains(&format!("--{long}")))
            .collect();
        assert!(missing.is_empty(), "not in nibwardend.1: {missing:?}");
    }
}
