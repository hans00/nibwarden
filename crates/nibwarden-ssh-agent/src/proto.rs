//! Wire format of draft-ietf-sshm-ssh-agent: `uint32 length || byte type || contents`.

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::Identity;

/// OpenSSH's own limit for one agent message.
pub const MAX_MESSAGE: usize = 256 * 1024;

pub const FAILURE: u8 = 5;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;

/// `SIGN_REQUEST` flags selecting the RSA signature hash.
pub const RSA_SHA2_256: u32 = 2;
pub const RSA_SHA2_512: u32 = 4;

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Identities,
    Sign {
        key_blob: Vec<u8>,
        data: Vec<u8>,
        flags: u32,
    },
    /// Anything else (add/remove keys, lock, extensions): answered with `FAILURE`.
    Other(u8),
}

#[derive(Debug, thiserror::Error)]
#[error("malformed agent message")]
pub struct Malformed;

/// Cursor over SSH wire encoding (RFC 4251 §5).
pub(crate) struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    pub fn u8(&mut self) -> Result<u8, Malformed> {
        let (&b, rest) = self.0.split_first().ok_or(Malformed)?;
        self.0 = rest;
        Ok(b)
    }

    pub fn u32(&mut self) -> Result<u32, Malformed> {
        let (head, rest) = self.0.split_first_chunk::<4>().ok_or(Malformed)?;
        self.0 = rest;
        Ok(u32::from_be_bytes(*head))
    }

    pub fn string(&mut self) -> Result<&'a [u8], Malformed> {
        let len = self.u32()? as usize;
        if len > self.0.len() {
            return Err(Malformed);
        }
        let (s, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(s)
    }
}

pub fn parse(message: &[u8]) -> Result<Request, Malformed> {
    let mut r = Reader(message);
    match r.u8()? {
        REQUEST_IDENTITIES => Ok(Request::Identities),
        SIGN_REQUEST => Ok(Request::Sign {
            key_blob: r.string()?.to_vec(),
            data: r.string()?.to_vec(),
            flags: r.u32()?,
        }),
        other => Ok(Request::Other(other)),
    }
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

pub fn identities_answer(identities: &[Identity]) -> Vec<u8> {
    let mut out = vec![IDENTITIES_ANSWER];
    out.extend_from_slice(&(identities.len() as u32).to_be_bytes());
    for id in identities {
        put_string(&mut out, &id.blob);
        put_string(&mut out, id.comment.as_bytes());
    }
    out
}

/// `signature` is an encoded SSH signature (`string algorithm || string blob`).
pub fn sign_response(signature: &[u8]) -> Vec<u8> {
    let mut out = vec![SIGN_RESPONSE];
    put_string(&mut out, signature);
    out
}

/// Reads one message body. `Ok(None)` on a clean end of stream between messages.
pub async fn read_message(
    stream: &mut (impl AsyncRead + Unpin),
) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match stream.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("agent message length {len} out of range"),
        ));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;
    Ok(Some(body))
}

pub async fn write_message(
    stream: &mut (impl AsyncWrite + Unpin),
    body: &[u8],
) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(4 + body.len());
    put_string(&mut frame, body);
    stream.write_all(&frame).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sign_request() {
        let mut msg = vec![SIGN_REQUEST];
        put_string(&mut msg, b"key");
        put_string(&mut msg, b"data");
        msg.extend_from_slice(&RSA_SHA2_256.to_be_bytes());
        assert_eq!(
            parse(&msg).unwrap(),
            Request::Sign {
                key_blob: b"key".to_vec(),
                data: b"data".to_vec(),
                flags: RSA_SHA2_256
            }
        );
    }

    #[test]
    fn rejects_truncated_and_overlong_strings() {
        let mut msg = vec![SIGN_REQUEST];
        msg.extend_from_slice(&100u32.to_be_bytes());
        msg.extend_from_slice(b"short");
        assert!(parse(&msg).is_err());
        assert!(parse(&[SIGN_REQUEST, 0, 0]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn unknown_messages_are_other() {
        // SSH_AGENTC_ADD_IDENTITY
        assert_eq!(parse(&[17, 1, 2, 3]).unwrap(), Request::Other(17));
    }
}
