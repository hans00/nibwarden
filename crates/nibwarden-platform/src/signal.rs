//! Requests to shut down.

/// Resolves when the process is asked to stop (Linux: SIGTERM or SIGINT).
pub async fn shutdown_requested() -> std::io::Result<()> {
    imp::shutdown_requested().await
}

#[cfg(target_os = "linux")]
mod imp {
    use tokio::signal::unix::{SignalKind, signal};

    pub async fn shutdown_requested() -> std::io::Result<()> {
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub async fn shutdown_requested() -> std::io::Result<()> {
        tokio::signal::ctrl_c().await
    }
}
