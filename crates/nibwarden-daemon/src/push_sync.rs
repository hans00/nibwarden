//! Live sync (phase2.md P2-M5): the server's notifications hub announces vault changes
//! made on other devices, and the daemon syncs within a second instead of waiting for the
//! periodic sync, which stays as the fallback.
//!
//! The hub speaks SignalR over a WebSocket with the MessagePack hub protocol, as the
//! official clients use it (no negotiate step): a JSON handshake, then binary frames, each
//! a varint length and one MessagePack array.

use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt as _, StreamExt as _};
use nibwarden_ipc::{ErrorCode, LockState};
use tokio_tungstenite::tungstenite::Message;

use crate::daemon::Daemon;

/// SignalR message types (`[type, …]`).
const INVOCATION: u64 = 1;
const PING: u64 = 6;
const CLOSE: u64 = 7;
/// Bitwarden push types: 0–10 are vault changes, 11 a forced logout.
const LAST_SYNC_TYPE: i64 = 10;
const LOG_OUT: i64 = 11;

const KEEPALIVE: Duration = Duration::from_secs(15);
/// Changes often come in bursts (e.g. importing many items): one sync for the lot.
const SETTLE: Duration = Duration::from_secs(1);

/// Runs forever: connects whenever logged in, reconnects with a growing pause.
pub async fn run(daemon: Arc<Daemon>) {
    let mut pause = Duration::from_secs(5);
    loop {
        if daemon.lock_state().await == LockState::LoggedOut {
            // Login wakes us; the timeout covers a login that raced this check.
            let _ =
                tokio::time::timeout(Duration::from_secs(10), daemon.logged_in.notified()).await;
            continue;
        }
        match listen(&daemon).await {
            Ok(()) => {
                tracing::debug!("notifications hub closed the connection");
                pause = Duration::from_secs(5);
            }
            Err(e) => tracing::debug!(error = e, "notifications hub unavailable"),
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_secs(300));
    }
}

/// One connection, until it ends.
async fn listen(daemon: &Arc<Daemon>) -> Result<(), String> {
    let endpoint = daemon.push_endpoint().await.map_err(|e| e.message)?;
    let url = format!(
        "{}?access_token={}",
        endpoint.url,
        endpoint.access_token.as_str()
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .map_err(|e| e.to_string())?;
    drop(url);
    socket
        .send(Message::text(
            "{\"protocol\":\"messagepack\",\"version\":1}\u{1e}",
        ))
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(url = endpoint.url, "listening for vault changes");
    // Catch up on whatever changed while we were not listening.
    if let Err(e) = daemon.sync(false).await {
        tracing::debug!(error = %e, "sync after connecting failed");
    }

    let mut keepalive = tokio::time::interval(KEEPALIVE);
    let mut sync_at: Option<tokio::time::Instant> = None;
    loop {
        let settle = async {
            match sync_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            message = socket.next() => {
                let data = match message {
                    None | Some(Ok(Message::Close(_))) => return Ok(()),
                    Some(Err(e)) => return Err(e.to_string()),
                    Some(Ok(Message::Binary(data))) => data,
                    // The handshake answer ("{}\x1e") and pings, which tungstenite answers.
                    Some(Ok(_)) => continue,
                };
                for frame in frames(&data) {
                    match notification(&frame, &endpoint.device_id) {
                        Some(Notification::Changed) => {
                            sync_at.get_or_insert(tokio::time::Instant::now() + SETTLE);
                        }
                        Some(Notification::LogOut) => {
                            tracing::info!("the server ended this session");
                            if let Err(e) = daemon.logout().await {
                                tracing::warn!(error = %e, "logout failed");
                            }
                            return Ok(());
                        }
                        Some(Notification::Closed) => return Ok(()),
                        None => {}
                    }
                }
            }
            _ = keepalive.tick() => {
                socket.send(Message::binary(encode(&rmpv::Value::Array(vec![PING.into()]))))
                    .await
                    .map_err(|e| e.to_string())?;
            }
            () = settle => {
                sync_at = None;
                match daemon.sync(false).await {
                    Ok(ran) => tracing::info!(ran, "synced after a change on another device"),
                    Err(e) if e.code == ErrorCode::LoggedOut => return Ok(()),
                    Err(e) => tracing::warn!(error = %e, "sync after a notification failed"),
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Notification {
    /// Something in the vault changed on another device.
    Changed,
    LogOut,
    /// The hub is closing the connection.
    Closed,
}

/// Reads one hub message: `[1, headers, id, "ReceiveMessage", [{Type, ContextId, …}]]`.
/// Changes made by this device itself (`ContextId`) are ignored.
fn notification(frame: &[u8], device_id: &str) -> Option<Notification> {
    let value = rmpv::decode::read_value(&mut &frame[..]).ok()?;
    let parts = value.as_array()?;
    match parts.first()?.as_u64()? {
        INVOCATION => {}
        CLOSE => return Some(Notification::Closed),
        _ => return None,
    }
    if parts.get(3)?.as_str()? != "ReceiveMessage" {
        return None;
    }
    let message = parts.get(4)?.as_array()?.first()?.as_map()?;
    let field = |name: &str| {
        message
            .iter()
            .find(|(k, _)| k.as_str() == Some(name))
            .map(|(_, v)| v)
    };
    if field("ContextId").and_then(rmpv::Value::as_str) == Some(device_id) {
        return None;
    }
    match field("Type")?.as_i64()? {
        LOG_OUT => Some(Notification::LogOut),
        kind if (0..=LAST_SYNC_TYPE).contains(&kind) => Some(Notification::Changed),
        _ => None,
    }
}

/// Splits a WebSocket message into hub frames: each is a varint length, then that many
/// bytes.
fn frames(mut data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let mut len = 0usize;
        let mut shift = 0;
        let mut used = 0;
        for byte in data.iter().take(5) {
            used += 1;
            len |= usize::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
        }
        let Some(frame) = data.get(used..used + len) else {
            break;
        };
        out.push(frame.to_vec());
        data = &data[used + len..];
    }
    out
}

fn encode(value: &rmpv::Value) -> Vec<u8> {
    let mut body = Vec::new();
    rmpv::encode::write_value(&mut body, value).expect("writing to a Vec");
    let mut out = Vec::new();
    let mut len = body.len();
    loop {
        let mut byte = (len & 0x7f) as u8;
        len >>= 7;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend(body);
    out
}

#[cfg(test)]
mod tests {
    use rmpv::Value as V;

    use super::*;

    fn update(kind: i64, context: Option<&str>) -> Vec<u8> {
        encode(&V::Array(vec![
            1.into(),
            V::Map(vec![]),
            V::Nil,
            "ReceiveMessage".into(),
            V::Array(vec![V::Map(vec![
                ("ContextId".into(), context.map_or(V::Nil, Into::into)),
                ("Type".into(), kind.into()),
                ("Payload".into(), V::Map(vec![])),
            ])]),
        ]))
    }

    #[test]
    fn reads_hub_messages() {
        let mut data = update(0, Some("other-device"));
        data.extend(update(5, Some("this-device")));
        data.extend(update(11, None));
        data.extend(encode(&V::Array(vec![6.into()])));
        let got: Vec<_> = frames(&data)
            .iter()
            .map(|f| notification(f, "this-device"))
            .collect();
        assert_eq!(
            got,
            [
                Some(Notification::Changed),
                None,
                Some(Notification::LogOut),
                None
            ]
        );
    }

    #[test]
    fn varint_lengths_round_trip() {
        let big = V::String("x".repeat(300).into());
        let data = encode(&big);
        assert_eq!(data[0] & 0x80, 0x80);
        let frames = frames(&data);
        assert_eq!(frames.len(), 1);
        assert_eq!(rmpv::decode::read_value(&mut &frames[0][..]).unwrap(), big);
    }
}
