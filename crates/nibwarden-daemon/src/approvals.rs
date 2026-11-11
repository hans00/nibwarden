//! Confirmation before use (phase2.md §4.2), modelled on 1Password's SSH agent: SSH
//! signatures (logins, git) are always confirmed; an approval then lasts until the vault
//! locks, until nibwardend stops, or for a set number of hours (those two outlive a lock,
//! so the next request only asks to unlock), for the requesting application (or one
//! terminal tab of it, or only the one request, as configured), or for every application
//! if the user ticked that box.
//! Secret reads by programs follow the same rules, but can be switched off, and always
//! end when the vault locks, like 1Password CLI's authorization.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use nibwarden_config::{Ask, Remember, RememberFor};
use nibwarden_ipc::{
    ErrorBody, ErrorCode,
    prompt::{Access, Requester, Scope},
};
use nibwarden_platform::peer::{App, Peer};

use crate::daemon::Daemon;

/// What an approval is for, by vault id: approving one key or item does not cover another
/// one with the same name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    SshKey {
        id: String,
    },
    /// Every SSH key: what a master password typed in a terminal authorizes.
    AnySshKey,
    Item {
        id: String,
        field: String,
    },
}

/// When a remembered approval ends (`remember_for`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    Lock,
    Quit,
    At(Instant),
}

/// Who an approval is for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Holder {
    /// "Approve for all applications".
    Everyone,
    App(App),
    Terminal(App, u64),
}

pub struct Approvals {
    config: Mutex<nibwarden_config::Approval>,
    remembered: Mutex<HashMap<(Holder, Target), Ends>>,
}

impl Approvals {
    pub fn new(config: nibwarden_config::Approval) -> Self {
        Self {
            config: Mutex::new(config),
            remembered: Mutex::new(HashMap::new()),
        }
    }

    /// New settings, e.g. after the settings window saved them.
    pub fn set_config(&self, config: nibwarden_config::Approval) {
        *self.config.lock().expect("poisoned") = config;
    }

    fn config(&self) -> nibwarden_config::Approval {
        *self.config.lock().expect("poisoned")
    }

    /// Forgets every approval, e.g. on logout.
    pub fn clear(&self) {
        self.remembered.lock().expect("poisoned").clear();
    }

    /// The vault locked: SSH key approvals given "until quit" or for a set time stay
    /// (1Password's "remember key approval"); every other approval ends.
    pub fn locked(&self) {
        self.remembered
            .lock()
            .expect("poisoned")
            .retain(|(_, target), ends| {
                matches!(target, Target::SshKey { .. } | Target::AnySshKey) && *ends != Ends::Lock
            });
    }

    /// How far an approval given now reaches, to tell the user in the window.
    pub fn scope(&self) -> Scope {
        match self.config().remember {
            Remember::Request => Scope::Request,
            Remember::Application => Scope::Application,
            Remember::Terminal => Scope::Terminal,
        }
    }

    /// The application `peer` runs in, as shown in windows.
    pub fn application(peer: &Peer) -> Option<Requester> {
        peer.app.as_ref().map(|app| Requester {
            name: app.name(),
            path: app.exe.as_ref().map(|p| p.display().to_string()),
            pid: Some(app.pid),
        })
    }

    /// The holder an approval for `peer` is filed under, per the setting; `None` when
    /// approvals are not remembered.
    fn holder(&self, peer: &Peer, remember: Remember) -> Option<Holder> {
        let app = peer.app.clone()?;
        match (remember, peer.terminal) {
            (Remember::Request, _) => None,
            (Remember::Application, _) | (Remember::Terminal, None) => Some(Holder::App(app)),
            (Remember::Terminal, Some(tty)) => Some(Holder::Terminal(app, tty)),
        }
    }

    /// Records an approval the user just gave for `target`.
    pub fn remember(&self, peer: &Peer, target: Target, all_applications: bool) {
        let holder = if all_applications {
            Some(Holder::Everyone)
        } else {
            self.holder(peer, self.config().remember)
        };
        if let Some(holder) = holder {
            let ends = self.ends();
            tracing::info!(?holder, ?target, ?ends, "approval remembered");
            self.remembered
                .lock()
                .expect("poisoned")
                .insert((holder, target), ends);
        }
    }

    /// A master password typed in `peer`'s terminal authorizes SSH signatures for that
    /// terminal session (its application, in that tab), whatever the setting: typing the
    /// password is the confirmation.
    pub fn authorize_terminal(&self, peer: &Peer) {
        if let Some(holder) = self.holder(peer, Remember::Terminal) {
            tracing::info!(?holder, "SSH signatures authorized from the terminal");
            let ends = self.ends();
            self.remembered
                .lock()
                .expect("poisoned")
                .insert((holder, Target::AnySshKey), ends);
        }
    }

    /// Whether signatures from `peer` were authorized for any key (`ssh.authorize`).
    pub fn ssh_authorized(&self, peer: &Peer) -> bool {
        self.is_remembered(peer, &Target::AnySshKey)
    }

    /// Whether `peer` holds an approval for `target`, e.g. one that outlived a lock.
    pub fn is_remembered(&self, peer: &Peer, target: &Target) -> bool {
        let mut targets = vec![target.clone()];
        if matches!(target, Target::SshKey { .. }) {
            targets.push(Target::AnySshKey);
        }
        let mut holders = vec![Holder::Everyone];
        if let Some(app) = &peer.app {
            holders.push(Holder::App(app.clone()));
            if let Some(tty) = peer.terminal {
                holders.push(Holder::Terminal(app.clone(), tty));
            }
        }
        let now = Instant::now();
        let mut remembered = self.remembered.lock().expect("poisoned");
        remembered.retain(|_, ends| !matches!(ends, Ends::At(until) if *until <= now));
        holders.iter().any(|holder| {
            targets
                .iter()
                .any(|t| remembered.contains_key(&(holder.clone(), t.clone())))
        })
    }

    /// When an approval given now ends.
    fn ends(&self) -> Ends {
        match self.config().remember_for {
            RememberFor::Lock => Ends::Lock,
            RememberFor::Quit => Ends::Quit,
            RememberFor::Hours(hours) => {
                Ends::At(Instant::now() + Duration::from_secs(u64::from(hours) * 3600))
            }
        }
    }

    /// Decides one request on an unlocked vault: by a remembered approval, or by asking
    /// in a window. `reprompt` items always take the master password instead.
    pub async fn approve(
        &self,
        daemon: &Arc<Daemon>,
        peer: &Peer,
        access: Access,
        target: Target,
        reprompt: bool,
    ) -> Result<(), ErrorBody> {
        if reprompt {
            return daemon.prompts.reprompt(daemon, peer, access).await;
        }
        let signature = matches!(target, Target::SshKey { .. } | Target::AnySshKey);
        if !signature {
            match self.config().read_secret {
                Ask::Allow => return Ok(()),
                Ask::Deny => return Err(denied("refused by the approval settings")),
                Ask::Ask => {}
            }
        }
        if self.is_remembered(peer, &target) {
            tracing::debug!(?target, "approved earlier");
            return Ok(());
        }
        let application = Self::application(peer);
        match daemon
            .prompts
            .approve(peer, access, application, self.scope())
            .await
        {
            Ok(all_applications) => {
                self.remember(peer, target, all_applications);
                Ok(())
            }
            Err(e) if e.code == ErrorCode::Unavailable && signature => Err(denied(&format!(
                "SSH signatures need your approval, but {}; for git, sign in a terminal",
                e.message
            ))),
            Err(e) if e.code == ErrorCode::Unavailable => match self.config().without_window {
                Ask::Allow => {
                    tracing::info!(
                        peer = peer.describe(),
                        reason = e.message,
                        "allowed without a window"
                    );
                    Ok(())
                }
                Ask::Ask | Ask::Deny => Err(denied(&format!(
                    "this needs a confirmation, but {}",
                    e.message
                ))),
            },
            Err(e) => Err(e),
        }
    }
}

pub fn requester(peer: &Peer) -> Requester {
    Requester {
        name: peer.name(),
        path: peer.exe.as_ref().map(|p| p.display().to_string()),
        pid: peer.pid,
    }
}

fn denied(message: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::Denied, message)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn peer(app_pid: i32, tty: Option<u64>) -> Peer {
        let mut peer = Peer::synthetic(100, Some("/usr/bin/ssh".into()));
        peer.app = Some(App {
            pid: app_pid,
            started: 7,
            exe: Some(PathBuf::from("/usr/bin/terminal")),
        });
        peer.terminal = tty;
        peer
    }

    fn approvals(remember: Remember) -> Approvals {
        Approvals::new(nibwarden_config::Approval {
            remember,
            ..Default::default()
        })
    }

    #[test]
    fn remembers_per_application_until_cleared() {
        let approvals = approvals(Remember::Application);
        let key = Target::SshKey { id: "a".into() };
        approvals.remember(&peer(1, Some(5)), key.clone(), false);
        // Same application, another tab: still approved.
        assert!(approvals.is_remembered(&peer(1, Some(6)), &key));
        // Another application, or another key: not.
        assert!(!approvals.is_remembered(&peer(2, Some(5)), &key));
        assert!(!approvals.is_remembered(&peer(1, Some(5)), &Target::SshKey { id: "b".into() }));
        approvals.locked();
        assert!(!approvals.is_remembered(&peer(1, Some(5)), &key));
    }

    #[test]
    fn timed_and_quit_ssh_approvals_outlive_a_lock() {
        let item = Target::Item {
            id: "i".into(),
            field: "password".into(),
        };
        for remember_for in [RememberFor::Quit, RememberFor::Hours(4)] {
            let approvals = approvals(Remember::Application);
            approvals.set_config(nibwarden_config::Approval {
                remember_for,
                ..Default::default()
            });
            let key = Target::SshKey { id: "a".into() };
            approvals.remember(&peer(1, None), key.clone(), false);
            approvals.remember(&peer(1, None), item.clone(), false);
            approvals.locked();
            assert!(approvals.is_remembered(&peer(1, None), &key));
            // Secret reads end at lock, like 1Password CLI's authorization.
            assert!(!approvals.is_remembered(&peer(1, None), &item));
            approvals.clear();
            assert!(!approvals.is_remembered(&peer(1, None), &key));
        }
    }

    #[test]
    fn terminal_scope_and_all_applications() {
        let approvals = approvals(Remember::Terminal);
        let key = Target::SshKey { id: "a".into() };
        approvals.remember(&peer(1, Some(5)), key.clone(), false);
        assert!(approvals.is_remembered(&peer(1, Some(5)), &key));
        assert!(!approvals.is_remembered(&peer(1, Some(6)), &key));
        approvals.remember(&peer(1, Some(5)), key.clone(), true);
        assert!(approvals.is_remembered(&peer(9, None), &key));
    }

    #[test]
    fn request_scope_remembers_nothing_but_terminal_passwords_count() {
        let approvals = approvals(Remember::Request);
        let key = Target::SshKey { id: "a".into() };
        approvals.remember(&peer(1, Some(5)), key.clone(), false);
        assert!(!approvals.is_remembered(&peer(1, Some(5)), &key));
        approvals.authorize_terminal(&peer(1, Some(5)));
        assert!(approvals.is_remembered(&peer(1, Some(5)), &key));
        assert!(!approvals.is_remembered(&peer(1, Some(6)), &key));
    }

    #[test]
    fn timed_approvals_expire() {
        let approvals = approvals(Remember::Application);
        let key = Target::SshKey { id: "a".into() };
        let past = Instant::now().checked_sub(Duration::from_secs(1));
        approvals.remembered.lock().unwrap().insert(
            (Holder::App(peer(1, None).app.unwrap()), key.clone()),
            Ends::At(past.unwrap()),
        );
        assert!(!approvals.is_remembered(&peer(1, None), &key));
        approvals.set_config(nibwarden_config::Approval {
            remember_for: RememberFor::Hours(4),
            ..Default::default()
        });
        approvals.remember(&peer(1, None), key.clone(), false);
        assert!(approvals.is_remembered(&peer(1, None), &key));
    }
}
