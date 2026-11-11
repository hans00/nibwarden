use std::io;

use super::Peer;

pub enum ProcessHandle {}

impl std::fmt::Debug for ProcessHandle {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}

pub fn identify_owner(stream: &crate::transport::imp::Stream) -> io::Result<Option<Peer>> {
    match *stream {}
}
