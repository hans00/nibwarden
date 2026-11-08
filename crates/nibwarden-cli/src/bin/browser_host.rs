//! `nibwarden-browser-host`: the native messaging host the Bitwarden browser extension
//! starts as `com.8bit.bitwarden` (phase2.md §5.8), in place of Bitwarden's
//! `desktop_proxy`. It passes each message from the extension to nibwardend unchanged and
//! writes back the daemon's answer; the daemon does the protocol.
//!
//! Native messaging frames are a 4-byte length in native byte order, then JSON. The
//! browser passes the manifest path or the extension's origin as arguments; they are not
//! needed.

use std::{process::ExitCode, sync::Arc};

use nibwarden_cli::connect;
use nibwarden_ipc::Request;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::mpsc,
};

/// The most a browser accepts from a host in one message.
const MAX_MESSAGE: usize = 1024 * 1024;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let socket = std::env::var_os("NIBWARDEN_SOCKET").map(std::path::PathBuf::from);
    let socket: Arc<Option<std::path::PathBuf>> = Arc::new(socket);
    let (out, mut outgoing) = mpsc::channel::<Value>(16);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = outgoing.recv().await {
            let body = message.to_string().into_bytes();
            if body.len() > MAX_MESSAGE {
                continue;
            }
            let head = (body.len() as u32).to_ne_bytes();
            if stdout.write_all(&head).await.is_err()
                || stdout.write_all(&body).await.is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    });

    // As the desktop app's proxy does: "connected" once the daemon answers, else
    // "disconnected", which the extension shows as the desktop app not running.
    if let Err(e) = connect(socket.as_deref(), "nibwarden-browser-host").await {
        eprintln!("nibwarden-browser-host: {e:#}");
        let _ = out.send(json!({ "command": "disconnected" })).await;
        drop(out);
        let _ = writer.await;
        return ExitCode::FAILURE;
    }
    let _ = out.send(json!({ "command": "connected" })).await;

    let mut stdin = tokio::io::stdin();
    loop {
        let mut head = [0u8; 4];
        if stdin.read_exact(&mut head).await.is_err() {
            break; // The browser closed the port.
        }
        let len = u32::from_ne_bytes(head) as usize;
        if len > MAX_MESSAGE {
            eprintln!("nibwarden-browser-host: message of {len} bytes refused");
            break;
        }
        let mut body = vec![0u8; len];
        if stdin.read_exact(&mut body).await.is_err() {
            break;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&body) else {
            eprintln!("nibwarden-browser-host: a message that is not JSON");
            continue;
        };
        // One connection per message: unlocking waits for the user, and the extension may
        // ask for its status meanwhile.
        let socket = socket.clone();
        let out = out.clone();
        tokio::spawn(async move {
            let answer = match connect(socket.as_deref(), "nibwarden-browser-host").await {
                Ok(mut client) => client
                    .call::<Value>(Request::BrowserMessage { message })
                    .await
                    .map_err(anyhow::Error::from),
                Err(e) => Err(e),
            };
            match answer {
                Ok(Value::Null) => {}
                Ok(answer) => {
                    let _ = out.send(answer).await;
                }
                Err(e) => {
                    eprintln!("nibwarden-browser-host: {e:#}");
                    let _ = out.send(json!({ "command": "disconnected" })).await;
                }
            }
        });
    }
    ExitCode::SUCCESS
}
