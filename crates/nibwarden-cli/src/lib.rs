//! Shared by `nibwarden` and `nibwarden-ssh-sign`: reaching the daemon and unlocking on
//! the terminal.

use std::{path::Path, time::Duration};

use anyhow::{Context as _, Result, bail};
use nibwarden_ipc::{Client, ClientError, ErrorCode, Request, UnlockWith};
use nibwarden_platform::terminal;
use serde::de::DeserializeOwned;

/// Connects to the daemon as `name`; when the default socket is dead, asks systemd to
/// start `nibwardend.service` first.
pub async fn connect(socket: Option<&Path>, name: &str) -> Result<Client> {
    let (path, autostart) = match socket {
        Some(path) => (path.to_owned(), false),
        None => (
            nibwarden_ipc::default_socket_path().context(
                "no runtime directory for the socket (XDG_RUNTIME_DIR is unset); pass --socket",
            )?,
            true,
        ),
    };
    match Client::connect(&path, name).await {
        Err(ClientError::Connect { source, .. })
            if autostart
                && matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
        {
            if !nibwarden_platform::service::start_daemon().await {
                bail!(
                    "nibwardend is not running and could not be started by the service \
                     manager; start it with `nibwardend &`"
                );
            }
            for _ in 0..50 {
                if let Ok(client) = Client::connect(&path, name).await {
                    return Ok(client);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(Client::connect(&path, name).await?)
        }
        other => Ok(other?),
    }
}

/// Calls a method that needs the unlocked vault. If the vault is locked, the request is
/// sent again asking the daemon to unlock in its window; where no window can be shown (no
/// desktop, or over SSH), with the master password from the terminal if there is one.
/// Either way one prompt both unlocks and allows the request (phase2.md §5.3).
pub async fn call_vault<T: DeserializeOwned>(client: &mut Client, request: Request) -> Result<T> {
    match client.call(request.clone()).await {
        Err(ClientError::Remote(e)) if e.code == ErrorCode::Locked => {
            match client
                .call_unlocking(request.clone(), UnlockWith::Window)
                .await
            {
                Err(ClientError::Remote(why)) if why.code == ErrorCode::Unavailable => {
                    if terminal::available() {
                        let password =
                            terminal::read_password("Vault is locked. Master password: ")?;
                        let unlock = UnlockWith::Password { password };
                        return Ok(client.call_unlocking(request, unlock).await?);
                    }
                    // Neither: keep the plain `locked` error and its exit code.
                    Err(ClientError::Remote(e))
                        .context("run `nibwarden unlock` in a terminal first")
                }
                other => Ok(other?),
            }
        }
        other => Ok(other?),
    }
}
