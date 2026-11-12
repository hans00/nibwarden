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
