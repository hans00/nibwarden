use nibwarden_core::{Error, TwoFactorProvider};
use nibwarden_ipc::{ErrorBody, ErrorCode};

/// Maps a core error to its wire form. SDK details stay in the daemon log; the client
/// gets the short message.
pub fn to_ipc(error: Error) -> ErrorBody {
    let code = match &error {
        Error::TwoFactorRequired(providers) => {
            let mut body = ErrorBody::new(ErrorCode::TwoFactorRequired, error.to_string());
            body.providers = providers
                .iter()
                .map(|p| match p {
                    TwoFactorProvider::Authenticator => {
                        nibwarden_ipc::TwoFactorProvider::Authenticator
                    }
                    TwoFactorProvider::Email => nibwarden_ipc::TwoFactorProvider::Email,
                    TwoFactorProvider::NewDevice => nibwarden_ipc::TwoFactorProvider::NewDevice,
                })
                .collect();
            return body;
        }
        Error::LoggedOut | Error::SessionEnded => ErrorCode::LoggedOut,
        Error::Locked => ErrorCode::Locked,
        Error::WrongPassword | Error::Rejected(_) | Error::Denied(_) => ErrorCode::Denied,
        Error::NotFound(_) => ErrorCode::NotFound,
        Error::Ambiguous(_) => ErrorCode::Ambiguous,
        Error::Invalid(_) => ErrorCode::BadRequest,
        Error::Sdk { .. } | Error::State(_) | Error::Io(_) | Error::Corrupt(_) => {
            tracing::warn!(error = %error, "request failed");
            ErrorCode::Internal
        }
    };
    ErrorBody::new(code, error.to_string())
}
