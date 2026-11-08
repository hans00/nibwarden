//! What a signature will be used for, read from the data to be signed (phase1.md §8.2).
//! The client controls these bytes, so this is for display and logging, not for trust.

use crate::proto::Reader;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// Public-key user authentication (RFC 4252 §7), e.g. `ssh host`.
    Authentication {
        user: String,
    },
    /// `ssh-keygen -Y sign` (PROTOCOL.sshsig); git signs with namespace `git`.
    FileSignature {
        namespace: String,
    },
    Unknown,
}

const SSH_MSG_USERAUTH_REQUEST: u8 = 50;

impl Purpose {
    pub fn of(data: &[u8]) -> Self {
        if let Some(rest) = data.strip_prefix(b"SSHSIG") {
            return match Reader(rest).string() {
                Ok(ns) => Self::FileSignature {
                    namespace: String::from_utf8_lossy(ns).into_owned(),
                },
                Err(_) => Self::Unknown,
            };
        }
        let mut r = Reader(data);
        let user = (|| {
            r.string()?; // session identifier
            if r.u8()? != SSH_MSG_USERAUTH_REQUEST {
                return Err(crate::proto::Malformed);
            }
            r.string()
        })();
        match user {
            Ok(user) => Self::Authentication {
                user: String::from_utf8_lossy(user).into_owned(),
            },
            Err(_) => Self::Unknown,
        }
    }
}

impl std::fmt::Display for Purpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authentication { user } => write!(f, "ssh login as {user:?}"),
            Self::FileSignature { namespace } if namespace == "git" => f.write_str("git signing"),
            Self::FileSignature { namespace } => write!(f, "file signature ({namespace:?})"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(s: &[u8]) -> Vec<u8> {
        let mut out = (s.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(s);
        out
    }

    #[test]
    fn sshsig_namespace() {
        let mut data = b"SSHSIG".to_vec();
        data.extend(string(b"git"));
        data.extend(string(b""));
        data.extend(string(b"sha512"));
        data.extend(string(&[0; 64]));
        assert_eq!(
            Purpose::of(&data),
            Purpose::FileSignature {
                namespace: "git".into()
            }
        );
        assert_eq!(Purpose::of(&data).to_string(), "git signing");
    }

    #[test]
    fn userauth_user() {
        let mut data = string(&[7; 32]);
        data.push(SSH_MSG_USERAUTH_REQUEST);
        data.extend(string(b"alice"));
        data.extend(string(b"ssh-connection"));
        assert_eq!(
            Purpose::of(&data),
            Purpose::Authentication {
                user: "alice".into()
            }
        );
        assert_eq!(Purpose::of(b"random"), Purpose::Unknown);
    }
}
