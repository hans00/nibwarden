//! Daemon ↔ GUI prompt protocol (phase2.md §5.2). It runs over the stdin/stdout pipes of a
//! `nibwarden-gui prompt` process that the daemon started itself, never over the control
//! socket, so no other program can answer a prompt (phase2.md E3). Same framing as the
//! control protocol: 4-byte length, then JSON.
//!
//! Everything shown to the user is sent as data (names, codes), never as sentences, so
//! the GUI can present it in the user's language.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Daemon → GUI. The first message says what to ask; later ones react to answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Prompt {
    /// Ask for the master password to unlock. With `access`, unlocking also allows that
    /// request (phase2.md §5.3, "unlock and allow").
    Unlock {
        email: String,
        /// Programs waiting for the vault; may be empty.
        waiting: Vec<Requester>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        access: Option<Access>,
        /// With `access`: the application it comes from and how long allowing lasts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        application: Option<Requester>,
        #[serde(default)]
        scope: Scope,
        /// Ways to unlock besides the master password that work now.
        #[serde(default, skip_serializing_if = "QuickUnlock::is_none")]
        quick: QuickUnlock,
    },
    /// The vault is unlocked; ask whether to allow a request.
    Approve {
        access: Access,
        /// The application the requester runs in (e.g. the terminal), when known.
        application: Option<Requester>,
        scope: Scope,
    },
    /// The item asks for the master password again before it is used.
    Reprompt { email: String, access: Access },
    /// More programs started waiting while the window was open: the full list now.
    Waiting { waiting: Vec<Requester> },
    /// The last answer was not accepted; ask again. A reason code rather than text, so
    /// the GUI can show it in the user's language.
    Retry { reason: RetryReason },
}

/// How far allowing a request reaches (phase2.md §4.2); how long is `remember_for`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Only this request.
    Request,
    /// The application, until it quits.
    #[default]
    Application,
    /// The application in this terminal tab.
    Terminal,
}

/// Unlocking without the master password (phase2.md §12.19).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickUnlock {
    /// With the PIN (`Answer::Pin`).
    #[serde(default)]
    pub pin: bool,
    /// With the system's authentication (`Answer::System`).
    #[serde(default)]
    pub system: bool,
}

impl QuickUnlock {
    fn is_none(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryReason {
    WrongPassword,
    WrongPin,
    /// The PIN no longer works (too many wrong ones): the master password is needed.
    PinSuspended,
    /// The system did not confirm the user.
    NotConfirmed,
}

/// A program on whose behalf the daemon asks, as identified by the OS (not self-reported).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    /// Short name for display, e.g. `ssh`.
    pub name: String,
    /// Full executable path, if known.
    pub path: Option<String>,
    pub pid: Option<i32>,
}

/// What a program asks to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    pub requester: Requester,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Sign with the SSH key named `key`.
    SshSign { key: String, purpose: SignPurpose },
    /// Read `field` of the item named `item`; `all` for the whole item (to edit it).
    ReadSecret { item: String, field: String },
    /// Change the vault: `item` is the item's name (a new item's too).
    ChangeItem { item: String, change: Change },
    /// Use the Send named `send` (a new Send's name too).
    Send { send: String, what: SendUse },
    /// Export the whole vault (`format` as in [`crate::ExportFormatData::as_str`]).
    Export { format: String },
    /// Import from the file `source`: `items` of them, when known beforehand.
    Import { source: String, items: Option<u32> },
    /// The Bitwarden browser extension asks to be unlocked with the vault's key, or only to
    /// confirm that it is the user (when its desktop unlock is switched on).
    Browser { what: BrowserUse },
}

/// What the browser extension asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserUse {
    Unlock,
    Verify,
}

/// What a program does with a Send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendUse {
    /// Read its link and text.
    Read,
    Create,
    Delete,
}

impl SendUse {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Create,
    Edit,
    Favorite,
    Trash,
    Restore,
    Delete,
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Edit => "edit",
            Self::Favorite => "favorite",
            Self::Trash => "trash",
            Self::Restore => "restore",
            Self::Delete => "delete",
        }
    }
}

/// What a signature is for, as the signed data claims (the requester controls it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignPurpose {
    /// SSH login as `user`.
    Login {
        user: String,
    },
    /// A git commit or tag (SSHSIG namespace `git`).
    Git,
    /// Another SSHSIG namespace.
    File {
        namespace: String,
    },
    Unknown,
}

/// GUI → daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    /// To `unlock` and `reprompt`.
    Password {
        password: Zeroizing<String>,
        /// "Approve for all applications" (as long as `remember_for`); unlock-and-allow only.
        #[serde(default)]
        all_applications: bool,
    },
    /// To `unlock`: the PIN instead of the master password.
    Pin {
        pin: Zeroizing<String>,
        #[serde(default)]
        all_applications: bool,
    },
    /// To `unlock`: confirm the user with the system's authentication instead.
    System {
        #[serde(default)]
        all_applications: bool,
    },
    /// To `approve`.
    Allow {
        #[serde(default)]
        all_applications: bool,
    },
    /// The user declined, cancelled or closed the window.
    Cancel,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wire_format() {
        let ssh = Requester {
            name: "ssh".into(),
            path: Some("/usr/bin/ssh".into()),
            pid: Some(1),
        };
        let prompt = Prompt::Unlock {
            email: "a@example.test".into(),
            waiting: vec![ssh.clone()],
            access: None,
            application: None,
            scope: Scope::Application,
            quick: QuickUnlock::default(),
        };
        assert_eq!(
            serde_json::to_value(&prompt).unwrap(),
            json!({"kind": "unlock", "email": "a@example.test", "scope": "application",
                   "waiting": [{"name": "ssh", "path": "/usr/bin/ssh", "pid": 1}]})
        );
        let approve = Prompt::Approve {
            access: Access {
                requester: ssh,
                action: Action::SshSign {
                    key: "Work".into(),
                    purpose: SignPurpose::Login { user: "git".into() },
                },
            },
            application: None,
            scope: Scope::Terminal,
        };
        assert_eq!(
            serde_json::to_value(&approve).unwrap()["access"]["action"],
            json!({"type": "ssh_sign", "key": "Work", "purpose": {"kind": "login", "user": "git"}})
        );
        let answer: Answer =
            serde_json::from_value(json!({"answer": "password", "password": "pw"})).unwrap();
        assert!(
            matches!(answer, Answer::Password { password, all_applications: false } if *password == "pw")
        );
        let allow: Answer =
            serde_json::from_value(json!({"answer": "allow", "all_applications": true})).unwrap();
        assert!(matches!(
            allow,
            Answer::Allow {
                all_applications: true
            }
        ));
        let cancel: Answer = serde_json::from_value(json!({"answer": "cancel"})).unwrap();
        assert!(matches!(cancel, Answer::Cancel));
    }
}
