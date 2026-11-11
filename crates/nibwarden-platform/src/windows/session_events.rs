// Not ported yet: reports Unsupported like other platforms.
use super::SessionEvent;

pub async fn watch(_: tokio::sync::mpsc::UnboundedSender<SessionEvent>) -> std::io::Result<()> {
    Err(crate::unsupported())
}
