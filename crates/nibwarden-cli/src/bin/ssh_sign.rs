//! `nibwarden-ssh-sign`: git's `gpg.ssh.program` (phase1.md §8.3). Runs `ssh-keygen` with
//! its arguments unchanged, but with `SSH_AUTH_SOCK` pointing at nibwarden's agent, so
//! signing works whatever agent the rest of the session uses. Before a signature it makes
//! sure the vault is unlocked, prompting on the terminal when there is one; verification
//! (`-Y verify`, `-Y find-principals`, …) needs no vault and goes straight to `ssh-keygen`.

use std::{ffi::OsString, process::ExitCode};

use anyhow::{Context as _, Result};
use nibwarden_cli::connect;
use nibwarden_ipc::{CanPromptResult, LockState, Request, StatusResult};
use nibwarden_platform::{process, terminal};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut keygen = std::process::Command::new("ssh-keygen");
    keygen.args(&args);
    if is_sign(&args) {
        match agent_socket().await {
            Ok(socket) => {
                keygen.env("SSH_AUTH_SOCK", socket);
            }
            Err(e) => {
                eprintln!("nibwarden-ssh-sign: {e:#}");
                return ExitCode::FAILURE;
            }
        }
    }
    let error = process::exec(&mut keygen);
    eprintln!("nibwarden-ssh-sign: cannot run ssh-keygen: {error}");
    ExitCode::FAILURE
}

/// `-Y sign` or `-Ysign`.
fn is_sign(args: &[OsString]) -> bool {
    args.iter().any(|a| a == "-Ysign") || args.windows(2).any(|w| w[0] == "-Y" && w[1] == "sign")
}

/// The agent socket. SSH signatures always need the user's approval (phase2.md §4.2): if
/// the daemon can show its window, the agent asks there once the signature is requested
/// (unlocking too, if needed); otherwise the master password typed in this terminal
/// authorizes signing for this terminal session, unless that was done already.
async fn agent_socket() -> Result<String> {
    let socket = std::env::var_os("NIBWARDEN_SOCKET").map(std::path::PathBuf::from);
    let mut client = connect(socket.as_deref(), "nibwarden-ssh-sign").await?;
    let status: StatusResult = client.call(Request::Status).await?;
    let agent = status
        .ssh_agent_socket
        .context("the SSH agent is disabled in nibwardend")?;
    if status.state == LockState::LoggedOut {
        anyhow::bail!("not logged in; run `nibwarden login`");
    }
    let window: CanPromptResult = client.call(Request::CanPrompt).await?;
    if !window.available && !window.ssh_authorized {
        if !terminal::available() {
            anyhow::bail!(
                "signing needs your approval, but there is neither a window nor a terminal to ask in"
            );
        }
        let password = terminal::read_password("nibwarden: master password to allow signing: ")?;
        client
            .call::<()>(Request::SshAuthorize { password })
            .await?;
    }
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sign() {
        let args = |s: &str| s.split(' ').map(OsString::from).collect::<Vec<_>>();
        assert!(is_sign(&args("-Y sign -n git -f key buf")));
        assert!(is_sign(&args("-Ysign -n git -f key buf")));
        assert!(!is_sign(&args("-Y verify -n git -f signers -I a -s sig")));
        assert!(!is_sign(&args("-Y find-principals -f signers -s sig")));
    }
}
