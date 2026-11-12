//! `nibwarden-gui tray` (phase2.md P2-M8): an icon in the system tray that shows whether
//! the vault is locked, with a menu to open the main window, quick access and the
//! settings, and to lock. It follows the daemon like the main window does (a status
//! request every few seconds) and ends when the setting is turned off.

use std::{cell::Cell, path::PathBuf, process::ExitCode, time::Duration};

use nibwarden_ipc::{Client, LockState, Request, StatusResult};
use nibwarden_platform::{
    paths,
    tray::{Chosen, Entry, Look, Tray},
};
use slint::ComponentHandle as _;
use tokio::sync::{mpsc, watch};

use crate::{TrayText, appearance, vault_ui};

/// How often the icon checks whether the vault locked or was unlocked elsewhere.
const REFRESH: Duration = Duration::from_secs(3);

/// The autostart entry's name, and what it runs.
pub const AUTOSTART: &str = "nibwarden-tray";
pub const AUTOSTART_ARG: &str = "--autostart";

/// The menu's texts in the chosen language, read from `TrayText`.
#[derive(Debug, Clone, Default)]
struct Texts {
    name: String,
    locked: String,
    unlocked: String,
    logged_out: String,
    not_running: String,
    open: String,
    quick: String,
    lock: String,
    unlock: String,
    settings: String,
    quit: String,
}

impl Texts {
    fn read(text: &TrayText) -> Self {
        Self {
            name: text.get_name().into(),
            locked: text.get_locked().into(),
            unlocked: text.get_unlocked().into(),
            logged_out: text.get_logged_out().into(),
            not_running: text.get_not_running().into(),
            open: text.get_open().into(),
            quick: text.get_quick().into(),
            lock: text.get_lock().into(),
            unlock: text.get_unlock().into(),
            settings: text.get_settings().into(),
            quit: text.get_quit().into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Open,
    Quick,
    Lock,
    Settings,
    Quit,
}

/// Starts the tray icon at login from now on, and right away (it does nothing when one
/// runs already); a running icon ends by itself when the setting is turned off.
pub fn set(on: bool) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let command = [
        exe.display().to_string(),
        "tray".to_owned(),
        AUTOSTART_ARG.to_owned(),
    ];
    nibwarden_platform::autostart::set(AUTOSTART, "nibwarden", &command, on)?;
    if on {
        let spawn = |mut command: std::process::Command| {
            command
                .arg("tray")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .spawn()
        };
        let mut child = match own_scope(&exe) {
            Some(scoped) => match spawn(scoped) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    spawn(std::process::Command::new(&exe))
                }
                started => started,
            },
            None => spawn(std::process::Command::new(&exe)),
        }?;
        std::thread::spawn(move || child.wait());
    }
    Ok(())
}

/// The icon outlives the window that started it, so it is not left in that window's
/// launch: GNOME tracks a launched app by its systemd scope (`app-gnome-nibwarden-….scope`)
/// and would keep the app in the dock while the icon runs. With a systemd user manager
/// (and `systemd-run`) it runs in a scope of its own.
fn own_scope(exe: &std::path::Path) -> Option<std::process::Command> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    if !std::path::Path::new(&dir).join("systemd/private").exists() {
        return None;
    }
    let mut command = std::process::Command::new("systemd-run");
    command
        .args(["--user", "--scope", "--quiet", "--collect", "--"])
        .arg(exe);
    Some(command)
}

/// With the tray setting on, brings the icon back if it was quit, and points the login
/// entry at this program (an update may have moved it, e.g. a new Nix store path).
/// Called whenever a nibwarden window opens.
pub fn restore() {
    if nibwarden_config::load().tray.enabled
        && let Err(e) = set(true)
    {
        tracing::warn!(error = %e, "cannot start the tray icon");
    }
}

/// `autostarted`: started at login, so only if the setting is still on.
pub fn run(autostarted: bool) -> ExitCode {
    let settings = nibwarden_config::load();
    if autostarted && !settings.tray.enabled {
        return ExitCode::SUCCESS;
    }
    // One icon; starting it again while it runs does nothing.
    let lock = paths::runtime_dir().and_then(|dir| {
        nibwarden_platform::fs::create_private_dir(&dir).ok()?;
        nibwarden_platform::fs::try_lock(&dir.join("tray.lock")).ok()
    });
    let Some(Some(_lock)) = lock else {
        return ExitCode::SUCCESS;
    };
    if let Err(e) = crate::windowless::select() {
        eprintln!("nibwarden-gui: {e}");
        return ExitCode::FAILURE;
    }
    let text = match TrayText::new() {
        Ok(text) => text,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            return ExitCode::FAILURE;
        }
    };
    appearance::select_language(&settings.appearance);
    let (texts_tx, texts) = watch::channel(Texts::read(&text));

    let worker = std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(serve(texts));
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });

    // Follow language changes, and end when the setting is turned off.
    let _follow = {
        let weak = text.as_weak();
        let enabled = Cell::new(settings.tray.enabled);
        appearance::follow_settings(move |appearance| {
            let now = nibwarden_config::load().tray.enabled;
            if enabled.replace(now) && !now {
                let _ = slint::quit_event_loop();
                return;
            }
            appearance::select_language(appearance);
            if let Some(text) = weak.upgrade() {
                let _ = texts_tx.send(Texts::read(&text));
            }
        })
    };
    let result = slint::run_event_loop_until_quit();
    drop(text);
    // Ends `serve`, which removes the icon.
    drop(_follow);
    let _ = worker.join();
    // macOS and Windows remove it on this thread (platform `main_thread`).
    nibwarden_platform::main_thread::wait(Some(Duration::ZERO));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Shows the icon and keeps it current until the texts' sender is gone or Quit is chosen.
async fn serve(mut texts: watch::Receiver<Texts>) {
    // Like the main window, the first connection starts the daemon if needed.
    let mut client = vault_ui::connect().await.ok();
    let mut status = status(&mut client).await;
    let (look, mut actions) = menu(&texts.borrow(), status.as_ref());
    let (chosen_tx, mut chosen) = mpsc::unbounded_channel();
    let tray = match Tray::show(look, move |c| {
        let _ = chosen_tx.send(c);
    })
    .await
    {
        Ok(tray) => tray,
        Err(e) => {
            eprintln!("nibwarden-gui: cannot show the tray icon: {e}");
            return;
        }
    };
    let mut shown = None;
    let mut ticks = tokio::time::interval(REFRESH);
    loop {
        tokio::select! {
            _ = ticks.tick() => {}
            changed = texts.changed() => {
                if changed.is_err() {
                    return;
                }
                shown = None;
            }
            Some(Chosen { entry, activation_token }) = chosen.recv() => {
                let action = match entry {
                    None => Some(Action::Open),
                    Some(index) => actions.get(index).copied().flatten(),
                };
                match action {
                    Some(Action::Quit) => return,
                    Some(Action::Lock) => {
                        if let Some(c) = client.as_mut() {
                            let _ = c.call::<()>(Request::Lock).await;
                        }
                    }
                    Some(Action::Open) => open("vault", activation_token),
                    Some(Action::Quick) => open("quick", activation_token),
                    Some(Action::Settings) => open("settings", activation_token),
                    None => {}
                }
            }
        }
        status = self::status(&mut client).await;
        let state = status.as_ref().map(|s| (s.state, s.email.clone()));
        if shown.as_ref() != Some(&state) {
            let look;
            (look, actions) = menu(&texts.borrow(), status.as_ref());
            tray.set(look).await;
            shown = Some(state);
        }
    }
}

/// The daemon's state; `None` when it is not running. Reconnects after a restart.
async fn status(client: &mut Option<Client>) -> Option<StatusResult> {
    if client.is_none() {
        let socket = std::env::var_os("NIBWARDEN_SOCKET")
            .map(PathBuf::from)
            .or_else(nibwarden_ipc::default_socket_path)?;
        *client = Client::connect(&socket, "nibwarden-gui").await.ok();
    }
    match client.as_mut()?.call(Request::Status).await {
        Ok(status) => Some(status),
        Err(_) => {
            *client = None;
            None
        }
    }
}

/// The icon and menu for `status`, and the action behind each menu entry.
fn menu(texts: &Texts, status: Option<&StatusResult>) -> (Look, Vec<Option<Action>>) {
    let state = status.map(|s| s.state);
    let detail = match state {
        None => &texts.not_running,
        Some(LockState::LoggedOut) => &texts.logged_out,
        Some(LockState::Locked) => &texts.locked,
        Some(LockState::Unlocked) => &texts.unlocked,
    };
    let mut entries = Vec::new();
    if let Some(email) = status.and_then(|s| s.email.clone()) {
        entries.push((Entry::Note(email), None));
    }
    entries.push((Entry::Note(detail.clone()), None));
    entries.push((Entry::Separator, None));
    let action = |label: &str, enabled: bool, action: Action| {
        (
            Entry::Action {
                label: label.to_owned(),
                enabled,
            },
            Some(action),
        )
    };
    entries.push(action(&texts.open, true, Action::Open));
    entries.push(action(&texts.quick, true, Action::Quick));
    entries.push(match state {
        // The main window shows the unlock form.
        Some(LockState::Locked) => action(&texts.unlock, true, Action::Open),
        _ => action(
            &texts.lock,
            state == Some(LockState::Unlocked),
            Action::Lock,
        ),
    });
    entries.push((Entry::Separator, None));
    entries.push(action(&texts.settings, true, Action::Settings));
    entries.push(action(&texts.quit, true, Action::Quit));
    let (menu, actions) = entries.into_iter().unzip();
    let look = Look {
        locked: state != Some(LockState::Unlocked),
        title: texts.name.clone(),
        detail: detail.clone(),
        menu,
    };
    (look, actions)
}

/// Starts another nibwarden window, passing on the desktop's permission to come to the
/// front.
fn open(window: &str, activation_token: Option<String>) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(exe);
    command.arg(window);
    if let Some(token) = activation_token {
        command.env("XDG_ACTIVATION_TOKEN", token);
    }
    match command.spawn() {
        // Collected when it ends, so it does not linger as a zombie.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(e) => tracing::warn!(window, error = %e, "cannot open a window"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts() -> Texts {
        Texts {
            lock: "Lock".into(),
            unlock: "Unlock…".into(),
            ..Default::default()
        }
    }

    fn status(state: LockState) -> StatusResult {
        StatusResult {
            state,
            email: Some("me@example.com".into()),
            server: None,
            last_sync: None,
            lock_timeout: None,
            ssh_agent_socket: None,
            pin_unlock: false,
            system_unlock: false,
            pin_set: false,
            session_ended: false,
        }
    }

    fn labels(look: &Look) -> Vec<(String, bool)> {
        look.menu
            .iter()
            .filter_map(|e| match e {
                Entry::Action { label, enabled } => Some((label.clone(), *enabled)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn menu_follows_the_vault() {
        let (look, actions) = menu(&texts(), Some(&status(LockState::Unlocked)));
        assert!(!look.locked);
        assert_eq!(look.menu[0], Entry::Note("me@example.com".into()));
        assert!(labels(&look).contains(&("Lock".into(), true)));
        assert_eq!(actions.len(), look.menu.len());
        let lock = look
            .menu
            .iter()
            .position(|e| matches!(e, Entry::Action { label, .. } if label == "Lock"))
            .unwrap();
        assert_eq!(actions[lock], Some(Action::Lock));

        let (look, actions) = menu(&texts(), Some(&status(LockState::Locked)));
        assert!(look.locked);
        let unlock = look
            .menu
            .iter()
            .position(|e| matches!(e, Entry::Action { label, .. } if label == "Unlock…"))
            .unwrap();
        assert_eq!(actions[unlock], Some(Action::Open));

        let (look, _) = menu(&texts(), None);
        assert!(look.locked);
        assert!(labels(&look).contains(&("Lock".into(), false)));
    }
}
