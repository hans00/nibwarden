//! The Bitwarden browser extension's desktop integration (phase2.md §5.8): nibwarden takes
//! the place of Bitwarden's desktop app, so the extension's "unlock with biometrics" asks
//! in nibwarden's window instead (unlocking nibwarden too, if it is locked) and receives
//! the vault's key. Messages arrive one by one through `nibwarden-browser-host`, which the
//! browser starts; the protocol is the desktop app's, as the extension speaks it.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use nibwarden_core::BrowserChannel;
use nibwarden_ipc::{
    ErrorBody, ErrorCode, LockState,
    prompt::{Access, Action, BrowserUse, Scope},
};
use nibwarden_platform::peer::Peer;
use serde_json::{Value, json};

use crate::{
    approvals::{Approvals, requester},
    daemon::Daemon,
    error::to_ipc,
    prompt_broker::Unlocked,
};

/// How old a message may be, as the desktop app allows.
const MESSAGE_VALID_MS: i64 = 10_000;

/// `BiometricsStatus` of the extension: unlocking through nibwarden can be used.
const STATUS_AVAILABLE: u8 = 0;
/// `BiometricsStatus.NotEnabledInConnectedDesktopApp`: nibwarden is not logged in to the
/// extension's account.
const STATUS_OTHER_ACCOUNT: u8 = 8;

/// Each extension's encrypted channel, by its `appId`; in memory only, as in the desktop
/// app, so the extension sets up a new one after the daemon restarts.
#[derive(Default)]
pub struct BrowserChannels(Mutex<HashMap<String, BrowserChannel>>);

impl BrowserChannels {
    pub fn clear(&self) {
        self.0.lock().expect("poisoned").clear();
    }
}

/// Answers one message from the extension; `Null` when it gets no answer.
pub async fn handle(daemon: &Arc<Daemon>, peer: &Peer, outer: Value) -> Result<Value, ErrorBody> {
    let app_id = outer
        .get("appId")
        .and_then(Value::as_str)
        .ok_or_else(|| ErrorBody::new(ErrorCode::BadRequest, "a message without appId"))?
        .to_owned();
    let Some(message) = outer.get("message") else {
        return Ok(Value::Null);
    };

    if message.get("command").and_then(Value::as_str) == Some("setupEncryption") {
        let (Some(public_key), Some(user_id)) = (
            message.get("publicKey").and_then(Value::as_str),
            message.get("userId").and_then(Value::as_str),
        ) else {
            return Ok(Value::Null);
        };
        if logged_in_user(daemon).await.as_deref() != Some(user_id) {
            tracing::info!("browser extension of another account");
            return Ok(json!({ "command": "wrongUserId", "appId": app_id }));
        }
        let (channel, shared_secret) = BrowserChannel::new(public_key).map_err(to_ipc)?;
        daemon
            .browser
            .0
            .lock()
            .expect("poisoned")
            .insert(app_id.clone(), channel);
        tracing::info!(peer = peer.describe(), "browser extension connected");
        return Ok(json!({
            "appId": app_id,
            "command": "setupEncryption",
            // Tells the extension this is a desktop app of the current protocol.
            "messageId": -1,
            "sharedSecret": shared_secret,
        }));
    }

    let opened = {
        let channels = daemon.browser.0.lock().expect("poisoned");
        channels.get(&app_id).map(|channel| channel.open(message))
    };
    let request: Value = match opened {
        Some(Ok(text)) => serde_json::from_str(&text).unwrap_or_default(),
        // No channel (e.g. after a restart), or one the extension no longer has.
        None | Some(Err(_)) => {
            return Ok(json!({ "command": "invalidateEncryption", "appId": app_id }));
        }
    };
    let timestamp = request.get("timestamp").and_then(Value::as_i64);
    if timestamp.is_none_or(|t| (t - now_ms()).abs() > MESSAGE_VALID_MS) {
        tracing::info!("browser extension message too old; ignored");
        return Ok(Value::Null);
    }
    let Some(command) = request.get("command").and_then(Value::as_str) else {
        return Ok(Value::Null);
    };
    let message_id = request.get("messageId").cloned().unwrap_or(Value::Null);
    let user_id = request.get("userId").and_then(Value::as_str);

    let mut reply = json!({ "command": command, "messageId": message_id });
    match command {
        "getBiometricsStatus" => reply["response"] = json!(STATUS_AVAILABLE),
        "getBiometricsStatusForUser" => {
            let ours = user_id.is_some() && logged_in_user(daemon).await.as_deref() == user_id;
            reply["response"] = json!(if ours {
                STATUS_AVAILABLE
            } else {
                STATUS_OTHER_ACCOUNT
            });
        }
        "canEnableBiometricUnlock" => reply["response"] = json!(true),
        "authenticateWithBiometrics" => {
            reply["response"] = json!(confirm(daemon, peer, BrowserUse::Verify).await);
        }
        "unlockWithBiometricsForUser" => {
            let ours = user_id.is_some() && logged_in_user(daemon).await.as_deref() == user_id;
            let key = if ours && confirm(daemon, peer, BrowserUse::Unlock).await {
                match daemon.in_use().await?.user_key().await {
                    Ok(key) => Some(key),
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot read the user key");
                        None
                    }
                }
            } else {
                None
            };
            reply["response"] = json!(key.is_some());
            if let Some(key) = key {
                tracing::info!(peer = peer.describe(), "browser extension unlocked");
                reply["userKeyB64"] = json!(key.as_str());
            }
        }
        other => {
            tracing::info!(command = other, "unknown browser extension command");
            return Ok(Value::Null);
        }
    }
    reply["timestamp"] = json!(now_ms());

    let sealed = {
        let channels = daemon.browser.0.lock().expect("poisoned");
        let Some(channel) = channels.get(&app_id) else {
            return Ok(json!({ "command": "invalidateEncryption", "appId": app_id }));
        };
        channel.seal(&reply.to_string()).map_err(to_ipc)?
    };
    Ok(json!({ "appId": app_id, "messageId": message_id, "message": sealed }))
}

/// Asks the user, every time: unlocking hands the extension the vault's key. On a locked
/// vault the unlock window shows the request, and unlocking there allows it.
async fn confirm(daemon: &Arc<Daemon>, peer: &Peer, what: BrowserUse) -> bool {
    // Shown as the browser that started the host, which is what the user recognises.
    let application = Approvals::application(peer);
    let access = Access {
        requester: application.clone().unwrap_or_else(|| requester(peer)),
        action: Action::Browser { what },
    };
    let result = if what == BrowserUse::Unlock && daemon.lock_state().await == LockState::Locked {
        match daemon
            .prompts
            .unlock_for(
                daemon,
                peer,
                access.clone(),
                application.clone(),
                Scope::Request,
            )
            .await
        {
            Ok(Unlocked::AndAllowed { .. }) => Ok(true),
            Ok(Unlocked::Already) => {
                daemon
                    .prompts
                    .approve(peer, access, application, Scope::Request)
                    .await
            }
            Err(e) => Err(e),
        }
    } else {
        daemon
            .prompts
            .approve(peer, access, application, Scope::Request)
            .await
    };
    result
        .inspect_err(|e| tracing::info!(?what, reason = e.message, "browser extension refused"))
        .is_ok()
}

async fn logged_in_user(daemon: &Daemon) -> Option<String> {
    daemon.account().await.map(|a| a.user_id().to_owned())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
