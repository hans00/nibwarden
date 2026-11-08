//! Serves the SSH agent socket from the daemon's vault (phase1.md §8.1).

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use nibwarden_core::SshKeyInfo;
use nibwarden_ipc::{
    ErrorBody, ErrorCode, LockState,
    prompt::{Access, Action, SignPurpose},
};
use nibwarden_platform::{
    peer::Peer,
    transport::{Listener, Stream},
};
use nibwarden_ssh_agent::{Backend, Identity, Purpose};
use zeroize::Zeroizing;

use crate::{
    approvals::{Approvals, Target, requester},
    daemon::Daemon,
    prompt_broker::Unlocked,
};

pub async fn serve(daemon: Arc<Daemon>, listener: Listener) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                tokio::spawn(handle_connection(daemon.clone(), stream));
            }
            Err(e) => {
                tracing::warn!(error = %e, "ssh agent accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle_connection(daemon: Arc<Daemon>, stream: Stream) {
    let Some(peer) = Peer::identify_owner(&stream) else {
        return;
    };
    tracing::debug!(peer = peer.describe(), "ssh agent client connected");
    let backend = AgentBackend { daemon, peer };
    if let Err(e) = nibwarden_ssh_agent::serve(stream, &backend).await {
        tracing::debug!(error = %e, "ssh agent connection ended with an error");
    }
}

struct AgentBackend {
    daemon: Arc<Daemon>,
    /// Pinned for the connection, so approval prompts name the real requester.
    peer: Peer,
}

impl AgentBackend {
    /// Makes sure the vault is unlocked, through the unlock window if one can be shown
    /// (phase2.md §4.1). `false` if it stays locked; when no window could be shown at
    /// all, the user is told by a notification instead.
    async fn unlocked(&self, what: &str) -> bool {
        match self.daemon.lock_state().await {
            LockState::Unlocked => return true,
            LockState::LoggedOut => return false,
            LockState::Locked => {}
        }
        tracing::info!(peer = self.peer.describe(), what, "vault is locked");
        match self.daemon.prompts.unlock(&self.daemon, &self.peer).await {
            Ok(()) => true,
            Err(e) => {
                self.locked_failure(&e);
                false
            }
        }
    }

    /// Tells the user why a key was not available, unless they just declined a window.
    fn locked_failure(&self, e: &ErrorBody) {
        if e.code == ErrorCode::Unavailable {
            tracing::debug!(reason = e.message, "no window");
            notify_locked(&self.peer.describe());
        } else {
            tracing::info!(reason = e.message, "vault stays locked");
        }
    }

    /// Vault keys paired with their agent identity, listed from the unlocked vault.
    async fn keys(&self) -> Vec<(Identity, SshKeyInfo)> {
        match self.daemon.ssh_keys().await {
            Ok(keys) => with_identities(keys),
            Err(e) => {
                tracing::warn!(error = %e, "cannot list SSH keys");
                Vec::new()
            }
        }
    }
}

fn with_identities(keys: Vec<SshKeyInfo>) -> Vec<(Identity, SshKeyInfo)> {
    keys.into_iter()
        .filter_map(
            |key| match Identity::from_openssh(&key.public_key, key.name.clone()) {
                Ok(identity) => Some((identity, key)),
                Err(e) => {
                    tracing::warn!(item = key.name, error = %e, "unusable SSH public key");
                    None
                }
            },
        )
        .collect()
}

fn sign_purpose(purpose: &Purpose) -> SignPurpose {
    match purpose {
        Purpose::Authentication { user } => SignPurpose::Login { user: user.clone() },
        Purpose::FileSignature { namespace } if namespace == "git" => SignPurpose::Git,
        Purpose::FileSignature { namespace } => SignPurpose::File {
            namespace: namespace.clone(),
        },
        Purpose::Unknown => SignPurpose::Unknown,
    }
}

#[async_trait::async_trait]
impl Backend for AgentBackend {
    async fn identities(&self) -> Vec<Identity> {
        let keys = match self.daemon.lock_state().await {
            LockState::Unlocked => self.keys().await,
            LockState::LoggedOut => Vec::new(),
            // Offer the keys seen when the vault was last unlocked: the master password
            // is asked for when one is used, in one window together with the request
            // (phase2.md §5.3). Public keys are not secret.
            LockState::Locked => match self.daemon.ssh_keys_seen() {
                Some(seen) => with_identities(seen),
                // Nothing seen since the daemon started: unlock to find out. ssh waits for
                // the answer without a timeout.
                None if self.unlocked("SSH keys requested").await => self.keys().await,
                None => Vec::new(),
            },
        };
        keys.into_iter().map(|(id, _)| id).collect()
    }

    async fn private_key(&self, key_blob: &[u8], purpose: &Purpose) -> Option<Zeroizing<String>> {
        let access = |key: &str| Access {
            requester: requester(&self.peer),
            action: Action::SshSign {
                key: key.to_owned(),
                purpose: sign_purpose(purpose),
            },
        };
        let mut allowed_by_unlock = None;
        match self.daemon.lock_state().await {
            LockState::Unlocked => {}
            LockState::LoggedOut => return None,
            LockState::Locked => {
                let name = self
                    .daemon
                    .ssh_keys_seen()
                    .and_then(|seen| {
                        with_identities(seen)
                            .into_iter()
                            .find(|(id, _)| id.blob == key_blob)
                    })
                    .map_or_else(|| "?".to_owned(), |(_, key)| key.name);
                tracing::info!(peer = self.peer.describe(), %purpose, "signature requested while locked");
                let application = Approvals::application(&self.peer);
                let scope = self.daemon.approvals.scope();
                match self
                    .daemon
                    .prompts
                    .unlock_for(&self.daemon, &self.peer, access(&name), application, scope)
                    .await
                {
                    Ok(Unlocked::AndAllowed { all_applications }) => {
                        allowed_by_unlock = Some(all_applications);
                    }
                    Ok(Unlocked::Already) => {}
                    Err(e) => {
                        self.locked_failure(&e);
                        return None;
                    }
                }
            }
        }

        let Some((_, key)) = self
            .keys()
            .await
            .into_iter()
            .find(|(id, _)| id.blob == key_blob)
        else {
            tracing::debug!("sign request for a key not in the vault");
            return None;
        };
        let target = Target::SshKey { id: key.id.clone() };
        match allowed_by_unlock {
            Some(all_applications) => {
                self.daemon
                    .approvals
                    .remember(&self.peer, target, all_applications);
            }
            None => {
                let decision = self
                    .daemon
                    .approvals
                    .approve(
                        &self.daemon,
                        &self.peer,
                        access(&key.name),
                        target,
                        key.reprompt,
                    )
                    .await;
                if let Err(e) = decision {
                    tracing::info!(key = key.name, %purpose, reason = e.message, "SSH signature denied");
                    return None;
                }
            }
        }
        match self.daemon.ssh_private_key(&key.id).await {
            Ok(pem) => {
                tracing::info!(
                    key = key.name,
                    %purpose,
                    peer = self.peer.describe(),
                    "SSH signature"
                );
                Some(pem)
            }
            Err(e) => {
                tracing::warn!(key = key.name, error = %e, "cannot read SSH private key");
                None
            }
        }
    }
}

/// One notification per this interval at most: one `ssh` or `git commit` can open several
/// agent connections in a row.
const NOTICE_INTERVAL: Duration = Duration::from_secs(10);

/// Best-effort desktop notification (phase1.md §8.1). Without a notification service (e.g.
/// a headless SSH login) it fails quietly and only the client's error remains.
fn notify_locked(requester: &str) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut last = LAST.lock().expect("poisoned");
        if last.is_some_and(|t| t.elapsed() < NOTICE_INTERVAL) {
            return;
        }
        *last = Some(Instant::now());
    }
    nibwarden_platform::notify::notify(
        "nibwarden is locked",
        &format!("{requester} wants an SSH key. Run `nibwarden unlock`."),
    );
}
