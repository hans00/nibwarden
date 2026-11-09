//! Confirming that the user is at the computer with the system's own authentication (the
//! login password, or a fingerprint where the system is set up for it), for unlocking
//! without the master password (phase2.md §12.19). Linux: polkit, with the action
//! nibwarden's packages install; the desktop's polkit agent shows the dialog.

use std::io;

/// The polkit action: `packaging/polkit/me.hans00.nibwarden.policy`.
pub const ACTION: &str = "me.hans00.nibwarden.unlock";

/// Whether the system can confirm the user for nibwarden (the action is installed).
/// Asked often (every status request); once found, the action is taken to stay.
pub async fn available() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FOUND: AtomicBool = AtomicBool::new(false);
    if FOUND.load(Ordering::Relaxed) {
        return true;
    }
    let found = imp::available().await;
    FOUND.store(found, Ordering::Relaxed);
    found
}

/// Asks the system to confirm the user, for the process `pid` (whose session's agent
/// shows the dialog); `true` if confirmed, `false` if cancelled or refused.
pub async fn authenticate(pid: u32) -> io::Result<bool> {
    imp::authenticate(pid).await
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{collections::HashMap, io};

    use zbus::{Connection, zvariant::Value};

    use super::ACTION;

    const POLKIT: &str = "org.freedesktop.PolicyKit1";
    const AUTHORITY: &str = "/org/freedesktop/PolicyKit1/Authority";
    const INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";
    /// `AllowUserInteraction`: the agent may ask.
    const ALLOW_INTERACTION: u32 = 1;

    async fn system() -> io::Result<Connection> {
        Connection::system().await.map_err(io::Error::other)
    }

    pub async fn available() -> bool {
        let Ok(bus) = system().await else {
            return false;
        };
        let reply = bus
            .call_method(
                Some(POLKIT),
                AUTHORITY,
                Some(INTERFACE),
                "EnumerateActions",
                &("",),
            )
            .await;
        let Ok(reply) = reply else {
            return false;
        };
        // a(ssssssuuua{ss}): the action id comes first.
        type Action = (
            String,
            String,
            String,
            String,
            String,
            String,
            u32,
            u32,
            u32,
            HashMap<String, String>,
        );
        reply
            .body()
            .deserialize::<Vec<Action>>()
            .is_ok_and(|actions| actions.iter().any(|a| a.0 == ACTION))
    }

    pub async fn authenticate(pid: u32) -> io::Result<bool> {
        let bus = system().await?;
        // A process subject; polkit looks its start time up when it is 0.
        let subject_details: HashMap<&str, Value<'_>> =
            HashMap::from([("pid", Value::from(pid)), ("start-time", Value::from(0u64))]);
        let subject = ("unix-process", subject_details);
        let details: HashMap<&str, &str> = HashMap::new();
        let reply = bus
            .call_method(
                Some(POLKIT),
                AUTHORITY,
                Some(INTERFACE),
                "CheckAuthorization",
                &(subject, ACTION, details, ALLOW_INTERACTION, ""),
            )
            .await
            .map_err(io::Error::other)?;
        authorized(&reply)
    }

    /// The `AuthorizationResult` (`(bba{ss})`): whether the subject is authorized.
    pub(super) fn authorized(reply: &zbus::Message) -> io::Result<bool> {
        let (authorized, _challenge, _details): (bool, bool, HashMap<String, String>) =
            reply.body().deserialize().map_err(io::Error::other)?;
        Ok(authorized)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::io;

    pub async fn available() -> bool {
        false
    }

    pub async fn authenticate(_: u32) -> io::Result<bool> {
        Err(crate::unsupported())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::collections::HashMap;

    /// What polkitd answers after the user confirmed, and when they did not.
    #[test]
    fn reads_the_authorization_result() {
        let call = zbus::Message::method_call(
            "/org/freedesktop/PolicyKit1/Authority",
            "CheckAuthorization",
        )
        .unwrap()
        .build(&())
        .unwrap();
        for (authorized, details) in [
            (true, HashMap::new()),
            (false, HashMap::from([("polkit.dismissed", "true")])),
        ] {
            let reply = zbus::Message::method_return(&call.header())
                .unwrap()
                .build(&((authorized, !authorized, details),))
                .unwrap();
            assert_eq!(super::imp::authorized(&reply).unwrap(), authorized);
        }
    }
}
