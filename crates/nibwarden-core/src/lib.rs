//! nibwarden-core wraps bitwarden `sdk-internal`.
//!
//! sdk-internal is explicitly unstable, so every `bitwarden_*` import lives in this crate.
//! Other crates only see the types defined here.

mod account;
mod browser;
mod cipher_sync;
mod edit;
mod error;
mod file_repository;
mod generator;
mod icons;
mod login;
mod platform;
mod registration;
mod send;
mod server;
mod store;
mod transfer;
mod vault;

pub use account::{Account, PushEndpoint, TwoFactor, TwoFactorProvider};
pub use browser::{BrowserChannel, ExtensionKeys};
pub use edit::{CustomField, CustomFieldKind, FolderInfo, ItemContent, KINDS, SECRET_KEYS};
pub use error::Error;
pub use generator::{PassphraseOptions, PasswordOptions, generate_passphrase, generate_password};
pub use icons::{fetch_icon, icons_url, valid_domain};
pub use registration::{
    Registration, finish_registration, make_registration, normalize_email, start_registration,
};
pub use send::{
    MAX_SEND_DAYS, MAX_SEND_FILE, NewSend, ReceivedSend, SendBody, SendInfo, SendSecret,
    decrypt_send_file, parse_send_link, receive_send,
};
pub use server::Server;
pub use store::Store;
pub use transfer::{Export, ExportFormat, ImportFile, Imported, MAX_IMPORT_FILE, read_import};
pub use vault::{
    FieldInfo, ItemDetails, ItemQuery, ItemRef, ItemSummary, NewLogin, Secret, SshKeyInfo, Totp,
};
