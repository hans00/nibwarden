use std::path::PathBuf;

pub use nibwarden_platform::paths::runtime_dir;

pub fn default_socket_path() -> Option<PathBuf> {
    runtime_dir().map(|dir| dir.join("daemon.sock"))
}
