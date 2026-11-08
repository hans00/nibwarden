//! SSH agent server (phase1.md §8). Keys are never added to the agent: the daemon's vault
//! is the only source, reached through [`Backend`]. This crate does not know about the
//! vault, locking or policy; it only speaks the protocol and signs.

mod proto;
mod purpose;
mod sign;

use tokio::io::{AsyncRead, AsyncWrite};
use zeroize::Zeroizing;

pub use proto::MAX_MESSAGE;
pub use purpose::Purpose;
pub use sign::{SignError, sign};

/// A public key offered to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// SSH wire encoding of the public key (RFC 4253 §6.6), as it appears in requests.
    pub blob: Vec<u8>,
    pub comment: String,
}

impl Identity {
    /// From an `authorized_keys`-style line (`ssh-ed25519 AAAA… [comment]`); the comment in
    /// the line is replaced by `comment`.
    pub fn from_openssh(public_key: &str, comment: impl Into<String>) -> Result<Self, SignError> {
        let key = ssh_key::PublicKey::from_openssh(public_key.trim())?;
        Ok(Self {
            blob: key.to_bytes()?,
            comment: comment.into(),
        })
    }
}

#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    /// Keys to offer. Empty while locked, so clients fall back to other methods.
    async fn identities(&self) -> Vec<Identity>;

    /// The OpenSSH-format private key matching `key_blob`, or `None` to refuse (unknown
    /// key, locked, or denied). The backend does its own authorisation here.
    async fn private_key(&self, key_blob: &[u8], purpose: &Purpose) -> Option<Zeroizing<String>>;
}

/// Serves one client connection until it closes. Errors are I/O or framing failures; any
/// request the agent cannot or will not fulfil is answered with `SSH_AGENT_FAILURE`.
pub async fn serve<S, B>(mut stream: S, backend: &B) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    B: Backend + ?Sized,
{
    while let Some(message) = proto::read_message(&mut stream).await? {
        let reply = match proto::parse(&message) {
            Ok(proto::Request::Identities) => proto::identities_answer(&backend.identities().await),
            Ok(proto::Request::Sign {
                key_blob,
                data,
                flags,
            }) => {
                let purpose = Purpose::of(&data);
                tracing::debug!(?purpose, flags, "sign request");
                match backend.private_key(&key_blob, &purpose).await {
                    Some(pem) => match sign(&pem, &key_blob, &data, flags) {
                        Ok(signature) => proto::sign_response(&signature),
                        Err(e) => {
                            tracing::warn!(error = %e, "signing failed");
                            vec![proto::FAILURE]
                        }
                    },
                    None => vec![proto::FAILURE],
                }
            }
            Ok(proto::Request::Other(kind)) => {
                tracing::debug!(kind, "unsupported agent request");
                vec![proto::FAILURE]
            }
            Err(e) => {
                tracing::debug!(error = %e, "malformed agent request");
                vec![proto::FAILURE]
            }
        };
        proto::write_message(&mut stream, &reply).await?;
    }
    Ok(())
}
