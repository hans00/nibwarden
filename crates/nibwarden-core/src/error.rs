use crate::TwoFactorProvider;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Only the providers nibwarden can complete are listed; never empty.
    #[error("two-step login required; available methods: {0:?}")]
    TwoFactorRequired(Vec<TwoFactorProvider>),
    #[error("not logged in")]
    LoggedOut,
    #[error("vault is locked")]
    Locked,
    #[error("wrong master password")]
    WrongPassword,
    /// The server refused the request (bad credentials, bad two-step code, ...); carries
    /// the server's own explanation.
    #[error("server refused: {0}")]
    Rejected(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Ambiguous(String),
    #[error("{0}")]
    Denied(String),
    /// Bad caller input (e.g. impossible generator options).
    #[error("{0}")]
    Invalid(String),
    #[error("{context}: {source}")]
    Sdk {
        context: &'static str,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("{0}")]
    State(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("corrupt state file: {0}")]
    Corrupt(#[from] serde_json::Error),
}

/// Attaches a short description of the SDK call that failed, so errors stay readable
/// without leaking SDK error types out of this crate.
pub(crate) trait SdkContext<T> {
    fn sdk(self, context: &'static str) -> Result<T, Error>;
}

impl<T, E> SdkContext<T> for Result<T, E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn sdk(self, context: &'static str) -> Result<T, Error> {
        self.map_err(|e| Error::Sdk {
            context,
            source: Box::new(e),
        })
    }
}
