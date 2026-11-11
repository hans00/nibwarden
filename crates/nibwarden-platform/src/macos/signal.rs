// Not ported yet: reports Unsupported like other platforms.
pub async fn shutdown_requested() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}
