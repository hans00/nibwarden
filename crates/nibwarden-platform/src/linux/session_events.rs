//! systemd-logind on the system bus: `Lock` and the `LockedHint` property of the
//! user's sessions (desktops set the hint when their lock screen comes up), and the
//! manager's `PrepareForSleep(true)`.

use std::{collections::HashMap, io};

use futures_util::StreamExt as _;
use tokio::sync::mpsc::UnboundedSender;
use zbus::{
    Connection, MatchRule, MessageStream,
    message::Type,
    zvariant::{OwnedObjectPath, OwnedValue},
};

use super::SessionEvent;

const LOGIND: &str = "org.freedesktop.login1";
const SESSION: &str = "org.freedesktop.login1.Session";

fn io_error(e: zbus::Error) -> io::Error {
    io::Error::other(e)
}

pub async fn watch(events: UnboundedSender<SessionEvent>) -> io::Result<()> {
    // `DBUS_SYSTEM_BUS_ADDRESS` is honoured, which the tests use for a mock logind.
    let bus = Connection::system().await.map_err(io_error)?;
    let rule = |interface: &'static str, member: &'static str| {
        MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(LOGIND)
            .and_then(|b| b.interface(interface))
            .and_then(|b| b.member(member))
            .map(|b| b.build())
    };
    let open = |rule: zbus::Result<MatchRule<'static>>| {
        let bus = bus.clone();
        async move { MessageStream::for_match_rule(rule?, &bus, Some(16)).await }
    };
    let mut lock = open(rule(SESSION, "Lock")).await.map_err(io_error)?;
    let mut hint = open(rule("org.freedesktop.DBus.Properties", "PropertiesChanged"))
        .await
        .map_err(io_error)?;
    let mut sleep = open(rule("org.freedesktop.login1.Manager", "PrepareForSleep"))
        .await
        .map_err(io_error)?;

    loop {
        let event = tokio::select! {
            Some(Ok(message)) = lock.next() => {
                let path = message.header().path().map(|p| p.to_string());
                mine(&bus, path.as_deref()).await.then_some(SessionEvent::ScreenLocked)
            }
            Some(Ok(message)) = hint.next() => {
                let body: Option<(String, HashMap<String, OwnedValue>, Vec<String>)> =
                    message.body().deserialize().ok();
                let locked = body.is_some_and(|(interface, changed, _)| {
                    interface == SESSION
                        && changed.get("LockedHint").and_then(|v| bool::try_from(v).ok()) == Some(true)
                });
                let path = message.header().path().map(|p| p.to_string());
                (locked && mine(&bus, path.as_deref()).await).then_some(SessionEvent::ScreenLocked)
            }
            Some(Ok(message)) = sleep.next() => {
                let starting: Option<(bool,)> = message.body().deserialize().ok();
                (starting == Some((true,))).then_some(SessionEvent::Sleeping)
            }
            else => return Ok(()),
        };
        if let Some(event) = event
            && events.send(event).is_err()
        {
            return Ok(());
        }
    }
}

/// Whether the session object at `path` belongs to the current user.
async fn mine(bus: &Connection, path: Option<&str>) -> bool {
    let Some(path) = path else { return false };
    // SAFETY: getuid has no failure mode.
    let user = format!("/org/freedesktop/login1/user/_{}", unsafe {
        libc::getuid()
    });
    let reply = bus
        .call_method(
            Some(LOGIND),
            user.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.User", "Sessions"),
        )
        .await;
    let sessions: Option<Vec<(String, OwnedObjectPath)>> = reply.ok().and_then(|reply| {
        let value: OwnedValue = reply.body().deserialize().ok()?;
        Vec::<(String, OwnedObjectPath)>::try_from(value).ok()
    });
    sessions.is_some_and(|list| list.iter().any(|(_, p)| p.as_str() == path))
}
