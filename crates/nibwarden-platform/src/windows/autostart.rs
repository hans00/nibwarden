use std::io;

use crate::winutil::run_key;

/// A value of the `Run` key of the current user, named `name` (`title` is for desktops
/// that show it; Windows shows the program's own name in Settings).
pub fn set(name: &str, _title: &str, command: &[String], on: bool) -> io::Result<()> {
    run_key::set(name, command, on)
}

pub fn is_set(name: &str) -> bool {
    run_key::is_set(name)
}
