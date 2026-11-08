//! Windows of `nibwarden-gui` (phase2.md §5). The binary is in `main.rs`; the library
//! exists so `examples/snapshot.rs` can render the same components.

slint::include_modules!();

mod account;
pub mod appearance;
mod editor;
pub mod prompt;
pub mod quick;
mod scan;
pub mod search;
mod sends;
pub mod settings;
mod transfer;
pub mod tray;
pub mod vault_ui;
pub mod vault_window;
