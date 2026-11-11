use std::io;

use super::{Look, OnChosen};

pub struct Tray;

impl Tray {
    pub async fn show(_: Look, _: OnChosen) -> io::Result<Self> {
        Err(crate::unsupported())
    }

    pub async fn set(&self, _: Look) {}
}
