//! Control protocol between `nibwardend` and its clients (phase1.md §6).
//!
//! Wire format: every message is a 4-byte big-endian length followed by that many bytes of
//! JSON. The first request on a connection must be [`Request::Hello`].

mod client;
mod framing;
mod paths;
pub mod prompt;
mod protocol;
mod reference;

pub use client::{Client, ClientError};
pub use framing::{FrameError, MAX_FRAME, read_frame, write_frame};
pub use paths::{default_socket_path, runtime_dir};
pub use protocol::*;
pub use reference::{Reference, ReferenceError};
