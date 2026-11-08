use std::path::Path;

use nibwarden_platform::transport::Stream;
use serde::de::DeserializeOwned;

use crate::{
    ErrorBody, FrameError, HelloResult, PROTOCOL_VERSION, Request, RequestFrame, ResponseBody,
    ResponseFrame, UnlockWith, read_frame, write_frame,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect to nibwardend at {path}")]
    Connect {
        path: String,
        source: std::io::Error,
    },
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The daemon handled the request and reported an error.
    #[error(transparent)]
    Remote(#[from] ErrorBody),
}

/// One connection to the daemon. Requests are sent one at a time.
pub struct Client {
    stream: Stream,
    next_id: u64,
    pub daemon_version: String,
}

impl Client {
    /// Connects and performs the `hello` handshake.
    pub async fn connect(path: &Path, client_name: &str) -> Result<Self, ClientError> {
        let stream = Stream::connect(path)
            .await
            .map_err(|source| ClientError::Connect {
                path: path.display().to_string(),
                source,
            })?;
        let mut client = Self {
            stream,
            next_id: 1,
            daemon_version: String::new(),
        };
        let hello: HelloResult = client
            .call(Request::Hello {
                client: client_name.to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            })
            .await?;
        if hello.protocol != PROTOCOL_VERSION {
            return Err(ClientError::Protocol(format!(
                "daemon speaks protocol {}, this client speaks {PROTOCOL_VERSION}; \
                 restart nibwardend after upgrading",
                hello.protocol
            )));
        }
        client.daemon_version = hello.daemon_version;
        Ok(client)
    }

    pub async fn call<T: DeserializeOwned>(&mut self, request: Request) -> Result<T, ClientError> {
        self.send(request, None).await
    }

    /// Like [`call`](Self::call), but if the vault is locked the daemon unlocks it first
    /// (`unlock`), and that unlock also allows this request.
    pub async fn call_unlocking<T: DeserializeOwned>(
        &mut self,
        request: Request,
        unlock: UnlockWith,
    ) -> Result<T, ClientError> {
        self.send(request, Some(unlock)).await
    }

    async fn send<T: DeserializeOwned>(
        &mut self,
        request: Request,
        unlock: Option<UnlockWith>,
    ) -> Result<T, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(
            &mut self.stream,
            &RequestFrame {
                v: PROTOCOL_VERSION,
                id,
                request,
                unlock,
            },
        )
        .await?;
        let response: ResponseFrame = read_frame(&mut self.stream).await?;
        if response.id != id {
            return Err(ClientError::Protocol(format!(
                "expected response {id}, got {}",
                response.id
            )));
        }
        match response.body {
            ResponseBody::Ok(value) => serde_json::from_value(value)
                .map_err(|e| ClientError::Protocol(format!("unexpected result shape: {e}"))),
            ResponseBody::Err(e) => Err(e.into()),
        }
    }
}
