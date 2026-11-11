pub async fn shutdown_requested() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}
