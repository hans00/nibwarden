//! The settings window's "SSH and Git" section: ssh using nibwarden's agent, and git
//! signing commits with a vault key (what `nibwarden ssh git-setup` does).

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use nibwarden_config::integration::{
    Overridden,
    git::{self, Scope},
    ssh,
};
use nibwarden_ipc::{Client, Request, SshKeyEntry, SshListResult, StatusResult, UnlockWith};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};

use crate::SettingsWindow;

/// The vault's SSH keys as last listed, in the order the window shows them.
type Keys = Arc<Mutex<Vec<SshKeyEntry>>>;

/// Shows what is in effect: a switch is on only when ssh or git really uses nibwarden.
pub fn show(ui: &SettingsWindow) {
    ui.set_ssh_agent(ssh::agent() == ssh::Agent::On);
    ui.set_ssh_other_agent(ssh::other_agent().unwrap_or_default().into());
    ui.set_git_signing(git::signing_key().is_some());
}

pub fn connect(ui: &SettingsWindow) {
    let keys: Keys = Arc::default();
    // The keys, if the vault is unlocked; never asks to unlock just to open the window.
    load_keys(ui, &keys, false, |_, _| {});

    let weak = ui.as_weak();
    ui.on_ssh_agent_changed(move |on| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_ssh_failed(SharedString::new());
        let done = if on {
            agent_socket().and_then(|socket| {
                ssh::set_agent(Some(&socket)).map_err(|e| match Overridden::of(&e) {
                    Some(o) => {
                        ui.set_ssh_failed(o.value.as_str().into());
                        String::new()
                    }
                    None => e.to_string(),
                })
            })
        } else {
            ssh::set_agent(None).map_err(|e| e.to_string())
        };
        match done {
            Err(e) if !e.is_empty() => {
                ui.set_error(format!("cannot change ~/.ssh/config: {e}").into())
            }
            _ => ui.set_error(SharedString::new()),
        }
        // Back to off if it did not take effect.
        show(&ui);
    });

    let weak = ui.as_weak();
    let all = keys.clone();
    ui.on_git_signing_changed(move |on| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_git_failed(SharedString::new());
        if !on {
            if let Err(e) = git::unset_signing() {
                ui.set_error(format!("cannot change the git config: {e}").into());
            }
            show(&ui);
            return;
        }
        // Listing the keys may need the vault unlocked: the unlock window opens.
        load_keys(&ui, &all, true, |ui, keys| {
            let chosen = ui.get_signing_key().max(0) as usize;
            match keys.get(chosen).or(keys.first()) {
                Some(key) => sign_with(ui, key),
                None => {
                    ui.set_error("The vault has no SSH keys to sign with.".into());
                    show(ui);
                }
            }
        });
    });

    let weak = ui.as_weak();
    ui.on_signing_key_chosen(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_signing_key(index);
        if let Some(key) = keys.lock().expect("poisoned").get(index as usize) {
            sign_with(&ui, key);
        }
    });
}

fn sign_with(ui: &SettingsWindow, key: &SshKeyEntry) {
    // Next to this program, as the CLI does; by name if it was moved.
    let helper = std::env::current_exe()
        .map(|exe| {
            exe.with_file_name(format!(
                "nibwarden-ssh-sign{}",
                std::env::consts::EXE_SUFFIX
            ))
        })
        .ok()
        .filter(|h| h.exists())
        .unwrap_or_else(|| PathBuf::from("nibwarden-ssh-sign"));
    ui.set_git_failed(SharedString::new());
    match git::set_signing(Scope::Global, &key.name, &key.public_key, &helper) {
        Ok(_) => ui.set_error(SharedString::new()),
        Err(e) => match Overridden::of(&e) {
            Some(o) => ui.set_git_failed(o.setting.as_str().into()),
            None => ui.set_error(e.to_string().into()),
        },
    }
    // Back to off if it did not take effect.
    show(ui);
}

/// The agent socket the daemon serves, else where it serves it by default.
fn agent_socket() -> Result<PathBuf, String> {
    let status = block_on(async {
        let mut client = connect_daemon().await?;
        client
            .call::<StatusResult>(Request::Status)
            .await
            .map_err(|e| e.to_string())
    })
    .ok()
    .flatten();
    if let Some(status) = status {
        return status
            .ssh_agent_socket
            .map(PathBuf::from)
            .ok_or_else(|| "nibwardend runs without its SSH agent".to_owned());
    }
    nibwarden_ipc::default_socket_path()
        .map(|p| nibwarden_platform::transport::endpoint(&p.with_file_name("ssh-agent.sock")))
        .ok_or_else(|| "no runtime directory".to_owned())
}

/// Lists the vault's SSH keys in the background (`unlock`: opening the unlock window if
/// the vault is locked), shows them, then calls `then` on the window.
fn load_keys(
    ui: &SettingsWindow,
    keys: &Keys,
    unlock: bool,
    then: impl FnOnce(&SettingsWindow, &[SshKeyEntry]) + Send + 'static,
) {
    let weak = ui.as_weak();
    let keys = keys.clone();
    std::thread::spawn(move || {
        let listed = block_on(async {
            let mut client = connect_daemon().await?;
            let result = if unlock {
                client
                    .call_unlocking::<SshListResult>(Request::SshList, UnlockWith::Window)
                    .await
            } else {
                client.call::<SshListResult>(Request::SshList).await
            };
            result.map_err(|e| e.to_string())
        });
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match listed {
                Ok(Some(list)) => {
                    let configured = git::signing_key();
                    let names: Vec<SharedString> =
                        list.keys.iter().map(|k| k.name.as_str().into()).collect();
                    let index = configured
                        .and_then(|name| list.keys.iter().position(|k| k.name == name))
                        .unwrap_or(0);
                    ui.set_ssh_keys(ModelRc::new(VecModel::from(names)));
                    ui.set_signing_key(index as i32);
                    *keys.lock().expect("poisoned") = list.keys.clone();
                    then(&ui, &list.keys);
                }
                // Locked and not asked to unlock, or no daemon: nothing to show yet.
                Ok(None) | Err(_) if !unlock => {}
                Ok(None) => show(&ui),
                Err(e) => {
                    ui.set_error(e.into());
                    show(&ui);
                }
            }
        });
    });
}

pub(crate) async fn connect_daemon() -> Result<Client, String> {
    let socket = std::env::var_os("NIBWARDEN_SOCKET")
        .map(PathBuf::from)
        .or_else(nibwarden_ipc::default_socket_path)
        .ok_or("no runtime directory")?;
    Client::connect(&socket, "nibwarden-gui")
        .await
        .map_err(|e| e.to_string())
}

/// Runs `work` on a runtime of its own; `Ok(None)` if no runtime could be made.
pub(crate) fn block_on<T>(
    work: impl Future<Output = Result<T, String>>,
) -> Result<Option<T>, String> {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Ok(None);
    };
    runtime.block_on(work).map(Some)
}
