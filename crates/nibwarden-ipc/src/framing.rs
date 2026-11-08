use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use zeroize::Zeroizing;

/// Largest accepted message body.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("peer closed the connection")]
    Closed,
    #[error("message of {0} bytes exceeds the {MAX_FRAME} byte limit")]
    TooLarge(usize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("malformed message: {0}")]
    Json(#[from] serde_json::Error),
}

/// Reads one message. The raw bytes may hold secrets (master passwords travel this way),
/// so the buffer is wiped once decoded.
pub async fn read_frame<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> Result<T, FrameError> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = Zeroizing::new(vec![0u8; len]);
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

pub async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &impl Serialize,
) -> Result<(), FrameError> {
    let body = Zeroizing::new(serde_json::to_vec(message)?);
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(body.len()));
    }
    let mut frame = Zeroizing::new(Vec::with_capacity(4 + body.len()));
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let writer = tokio::spawn(async move {
            write_frame(&mut a, &serde_json::json!({"hello": "世界"}))
                .await
                .unwrap();
        });
        let got: serde_json::Value = read_frame(&mut b).await.unwrap();
        writer.await.unwrap();
        assert_eq!(got["hello"], "世界");
    }

    #[tokio::test]
    async fn rejects_oversized_length_before_allocating() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
        let err = read_frame::<serde_json::Value>(&mut b).await.unwrap_err();
        assert!(matches!(err, FrameError::TooLarge(_)));
    }

    #[tokio::test]
    async fn eof_is_closed() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        let err = read_frame::<serde_json::Value>(&mut b).await.unwrap_err();
        assert!(matches!(err, FrameError::Closed));
    }
}
