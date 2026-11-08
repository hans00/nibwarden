use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Bumped on any incompatible change to the messages below.
pub const PROTOCOL_VERSION: u32 = 1;

/// `{ "v": 1, "id": 42, "method": "...", "params": { ... } }`
#[derive(Debug, Serialize, Deserialize)]
pub struct RequestFrame {
    pub v: u32,
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
    /// If the vault is locked when the request runs, unlock it this way first instead of
    /// failing with `locked`. Unlocking for a request also allows that request: the user
    /// just proved their presence with the master password (phase2.md §5.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlock: Option<UnlockWith>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "with", rename_all = "snake_case")]
pub enum UnlockWith {
    /// The daemon's unlock window, showing this request. Fails with `unavailable` if no
    /// window can be shown for the caller.
    Window,
    /// A master password the client read from its terminal.
    Password { password: Zeroizing<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    /// Must be the first request on a connection. Result: [`HelloResult`].
    #[serde(rename = "hello")]
    Hello { client: String, version: String },
    /// Result: [`StatusResult`].
    #[serde(rename = "status")]
    Status,
    /// Result: [`StatusResult`]. Fails with [`ErrorCode::TwoFactorRequired`] when a
    /// second factor is needed; the connection then remembers the credentials so the
    /// client can follow up with `auth.send_email_code` / `auth.login_2fa`.
    #[serde(rename = "auth.login")]
    Login {
        server: String,
        email: String,
        password: Zeroizing<String>,
    },
    /// Asks the server to email a two-step code (or a new device code) for the pending
    /// login. Result: null.
    #[serde(rename = "auth.send_email_code")]
    SendEmailCode,
    /// Completes the pending login. Result: [`StatusResult`].
    #[serde(rename = "auth.login_2fa")]
    Login2fa {
        provider: TwoFactorProvider,
        token: Zeroizing<String>,
    },
    /// Starts creating an account (phase2.md §5.7). Result: [`RegisterStartResult`].
    #[serde(rename = "auth.register_start")]
    RegisterStart {
        server: String,
        email: String,
        #[serde(default)]
        name: String,
    },
    /// Creates the account with the token from `auth.register_start` or the emailed
    /// link; log in with `auth.login` afterwards. Result: null.
    #[serde(rename = "auth.register_finish")]
    RegisterFinish {
        server: String,
        email: String,
        token: String,
        password: Zeroizing<String>,
        #[serde(default)]
        hint: Option<String>,
    },
    /// Result: null.
    #[serde(rename = "auth.logout")]
    Logout,
    /// Result: null.
    #[serde(rename = "lock.unlock")]
    Unlock { password: Zeroizing<String> },
    /// Asks the daemon to show its unlock window (phase2.md §5) and waits until the
    /// user unlocks or gives up. Result: null; `denied` if cancelled, `unavailable` if
    /// there is no graphical session or GUI.
    #[serde(rename = "lock.unlock_prompt")]
    UnlockPrompt,
    /// One item without its secret values, for a detail view. Result: [`ItemView`].
    #[serde(rename = "item.view")]
    ItemView {
        folder: Option<String>,
        item: String,
    },
    /// Authorizes SSH signatures for the caller's application session with the master
    /// password typed in its terminal (unlocking if needed): for when no window can be
    /// shown, e.g. git signing over SSH (phase2.md §4.2). Result: null; `denied` for a
    /// wrong password.
    #[serde(rename = "ssh.authorize")]
    SshAuthorize { password: Zeroizing<String> },
    /// Read `config.toml` again after the settings changed. Result: null.
    #[serde(rename = "config.reload")]
    ReloadConfig,
    /// Whether the daemon can show a window for this caller (it has a desktop, and the
    /// caller is not in a remote session). Result: [`CanPromptResult`].
    #[serde(rename = "lock.can_prompt")]
    CanPrompt,
    /// Result: null.
    #[serde(rename = "lock.lock")]
    Lock,
    /// Result: [`SyncResult`].
    #[serde(rename = "sync")]
    Sync {
        #[serde(default)]
        force: bool,
    },
    /// Non-sensitive fields of matching items. Result: [`ItemListResult`].
    #[serde(rename = "item.list")]
    ItemList {
        /// Case-insensitive substring of name, subtitle or URI.
        #[serde(default)]
        search: Option<String>,
        #[serde(default)]
        folder: Option<String>,
        /// The items in the trash instead.
        #[serde(default)]
        trash: bool,
    },
    /// One field of one item. Result: [`ValueResult`].
    #[serde(rename = "item.get")]
    ItemGet {
        #[serde(default)]
        folder: Option<String>,
        item: String,
        field: String,
    },
    /// Current TOTP code of one item. Result: [`TotpResult`].
    #[serde(rename = "item.totp")]
    ItemTotp {
        #[serde(default)]
        folder: Option<String>,
        item: String,
    },
    /// Result: [`ValueResult`].
    #[serde(rename = "generate.password")]
    GeneratePassword(PasswordParams),
    /// Result: [`ValueResult`].
    #[serde(rename = "generate.passphrase")]
    GeneratePassphrase(PassphraseParams),
    /// Public keys served by the SSH agent. Result: [`SshListResult`].
    #[serde(rename = "ssh.list")]
    SshList,
    /// An item's whole content, secrets included, for editing (by id). Result:
    /// [`ItemData`].
    #[serde(rename = "item.content")]
    ItemContent { id: String },
    /// Creates (no id) or replaces an item. Result: [`SavedResult`].
    #[serde(rename = "item.save")]
    ItemSave { item: ItemData },
    /// Result: null.
    #[serde(rename = "item.favorite")]
    ItemFavorite { id: String, favorite: bool },
    /// Moves an item to the trash. Result: null.
    #[serde(rename = "item.trash")]
    ItemTrash { id: String },
    /// Takes an item out of the trash. Result: null.
    #[serde(rename = "item.restore")]
    ItemRestore { id: String },
    /// Deletes an item in the trash for good. Result: null.
    #[serde(rename = "item.delete")]
    ItemDelete { id: String },
    /// Result: [`FolderListResult`].
    #[serde(rename = "folder.list")]
    FolderList,
    /// Result: [`SendListResult`].
    #[serde(rename = "send.list")]
    SendList,
    /// A Send's link and text. Result: [`SendSecretResult`].
    #[serde(rename = "send.get")]
    SendGet { id: String },
    /// Result: [`SavedResult`].
    #[serde(rename = "send.create")]
    SendCreate { send: NewSendData },
    /// Result: null.
    #[serde(rename = "send.delete")]
    SendDelete { id: String },
    /// The personal vault as a file, after checking the master password again.
    /// Result: [`ExportResult`].
    #[serde(rename = "vault.export")]
    VaultExport {
        format: ExportFormatData,
        master_password: Zeroizing<String>,
    },
    /// Adds the items of a Bitwarden JSON or CSV export or a KeePass database (told apart
    /// by content). `name` is the file's name, for the confirmation; `password` opens a
    /// password-protected file. Result: [`ImportResult`].
    #[serde(rename = "vault.import")]
    VaultImport {
        name: String,
        #[serde(with = "base64_bytes")]
        data: Zeroizing<Vec<u8>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<Zeroizing<String>>,
    },
    /// One native message from the Bitwarden browser extension, passed on unchanged by
    /// `nibwarden-browser-host` (phase2.md §5.8). Result: the message to send back to the
    /// extension, or null for none.
    #[serde(rename = "browser.message")]
    BrowserMessage { message: serde_json::Value },
}

impl Request {
    pub fn method(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Status => "status",
            Self::Login { .. } => "auth.login",
            Self::SendEmailCode => "auth.send_email_code",
            Self::Login2fa { .. } => "auth.login_2fa",
            Self::RegisterStart { .. } => "auth.register_start",
            Self::RegisterFinish { .. } => "auth.register_finish",
            Self::Logout => "auth.logout",
            Self::Unlock { .. } => "lock.unlock",
            Self::UnlockPrompt => "lock.unlock_prompt",
            Self::CanPrompt => "lock.can_prompt",
            Self::ItemView { .. } => "item.view",
            Self::ReloadConfig => "config.reload",
            Self::SshAuthorize { .. } => "ssh.authorize",
            Self::Lock => "lock.lock",
            Self::Sync { .. } => "sync",
            Self::ItemList { .. } => "item.list",
            Self::ItemGet { .. } => "item.get",
            Self::ItemTotp { .. } => "item.totp",
            Self::GeneratePassword(_) => "generate.password",
            Self::GeneratePassphrase(_) => "generate.passphrase",
            Self::SshList => "ssh.list",
            Self::ItemContent { .. } => "item.content",
            Self::ItemSave { .. } => "item.save",
            Self::ItemFavorite { .. } => "item.favorite",
            Self::ItemTrash { .. } => "item.trash",
            Self::ItemRestore { .. } => "item.restore",
            Self::ItemDelete { .. } => "item.delete",
            Self::FolderList => "folder.list",
            Self::SendList => "send.list",
            Self::SendGet { .. } => "send.get",
            Self::SendCreate { .. } => "send.create",
            Self::SendDelete { .. } => "send.delete",
            Self::VaultExport { .. } => "vault.export",
            Self::VaultImport { .. } => "vault.import",
            Self::BrowserMessage { .. } => "browser.message",
        }
    }
}

/// `{ "v": 1, "id": 42, "result": ... }` or `{ "v": 1, "id": 42, "error": { ... } }`
#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseFrame {
    pub v: u32,
    pub id: u64,
    #[serde(flatten)]
    pub body: ResponseBody,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ResponseBody {
    #[serde(rename = "result")]
    Ok(serde_json::Value),
    #[serde(rename = "error")]
    Err(ErrorBody),
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    /// Set with [`ErrorCode::TwoFactorRequired`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<TwoFactorProvider>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            providers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Locked,
    LoggedOut,
    AlreadyLoggedIn,
    NotFound,
    Ambiguous,
    Denied,
    TwoFactorRequired,
    BadRequest,
    /// The request needs something this session lacks, e.g. a display for a window.
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TwoFactorProvider {
    Authenticator,
    Email,
    /// The code Bitwarden emails to confirm a device it does not know yet; the server sends
    /// it on its own, `auth.send_email_code` sends another.
    NewDevice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterStartResult {
    /// The token to finish with, when the server does not verify the address; `None`:
    /// it emailed a link instead.
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol: u32,
    pub daemon_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockState {
    LoggedOut,
    Locked,
    Unlocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResult {
    pub state: LockState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Unix seconds of the last successful sync since the daemon started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<u64>,
    /// Seconds of inactivity after which the vault locks itself; `None` = never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_timeout: Option<u64>,
    /// Path of the SSH agent socket, when the agent is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_agent_socket: Option<String>,
}

/// An item for a detail view: which secrets it has is said, their values are read with
/// `item.get` / `item.totp` when the user asks for them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemView {
    pub id: String,
    pub name: String,
    /// `login`, `note`, `card`, `identity`, `ssh_key` or `other`.
    pub kind: String,
    pub folder: Option<String>,
    pub username: Option<String>,
    pub has_password: bool,
    pub has_totp: bool,
    pub has_notes: bool,
    pub uris: Vec<String>,
    pub fields: Vec<FieldView>,
    /// Reading a secret of this item asks for the master password again.
    pub reprompt: bool,
    /// Card, identity and SSH key values by key (see [`ItemData::values`]), in form
    /// order; `None` for secret ones, read with `item.get` and the key as field name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<(String, Option<String>)>,
    #[serde(default)]
    pub favorite: bool,
    /// In the trash.
    #[serde(default)]
    pub trashed: bool,
    /// When the item was created and last changed (RFC 3339).
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub revised: String,
    /// When the login's password last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_revised: Option<String>,
    /// When each earlier password was replaced, newest first. The passwords are read with
    /// `item.get` and the field name `password_history_<index>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub password_history: Vec<String>,
}

/// The kinds of item that can be created and edited, with their value keys
/// ([`ItemData::values`]) in form order. nibwarden-core has the same table.
pub const ITEM_KINDS: &[(&str, &[&str])] = &[
    ("login", &["username", "password", "totp"]),
    ("note", &[]),
    (
        "card",
        &[
            "cardholder_name",
            "brand",
            "number",
            "exp_month",
            "exp_year",
            "code",
        ],
    ),
    (
        "identity",
        &[
            "title",
            "first_name",
            "middle_name",
            "last_name",
            "username",
            "company",
            "ssn",
            "passport_number",
            "license_number",
            "email",
            "phone",
            "address1",
            "address2",
            "address3",
            "city",
            "state",
            "postal_code",
            "country",
        ],
    ),
    ("ssh_key", &["private_key", "public_key", "fingerprint"]),
];

/// Value keys that hold secrets.
pub const SECRET_KEYS: &[&str] = &[
    "password",
    "totp",
    "number",
    "code",
    "ssn",
    "passport_number",
    "license_number",
    "private_key",
];

/// An item's whole content, secrets included: what the edit form shows and saves
/// (`item.content`, `item.save`). Empty strings mean "not set".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemData {
    /// `None` for a new item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `login`, `note`, `card`, `identity` or `ssh_key`.
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub favorite: bool,
    /// Ask for the master password again before the item's secrets are used.
    #[serde(default)]
    pub reprompt: bool,
    #[serde(default)]
    pub notes: Zeroizing<String>,
    /// Type-specific values by key, e.g. `username`, `password`, `totp` (login);
    /// `cardholder_name`, `brand`, `number`, `exp_month`, `exp_year`, `code` (card);
    /// `first_name`, `email`, `address1`, ... (identity); `private_key` (SSH key: empty
    /// on a new item generates an Ed25519 key; `public_key` and `fingerprint` are
    /// read-only).
    #[serde(default)]
    pub values: Vec<(String, Zeroizing<String>)>,
    /// Logins only.
    #[serde(default)]
    pub uris: Vec<String>,
    #[serde(default)]
    pub fields: Vec<CustomFieldData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomFieldData {
    pub name: String,
    pub value: Zeroizing<String>,
    pub kind: CustomFieldKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomFieldKind {
    Text,
    Hidden,
    Boolean,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedResult {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderEntry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderListResult {
    pub folders: Vec<FolderEntry>,
}

/// A Send, without its content or link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendEntry {
    pub id: String,
    pub name: String,
    /// `None` for a text Send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<String>,
    /// RFC 3339.
    pub deletion_date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration_date: Option<String>,
    pub access_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_access_count: Option<u32>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default)]
    pub hide_email: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendListResult {
    pub sends: Vec<SendEntry>,
}

/// What lets someone read a Send.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendSecretResult {
    pub link: Zeroizing<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Zeroizing<String>>,
    #[serde(default)]
    pub hidden: bool,
}

/// A new Send.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSendData {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub body: SendBodyData,
    /// The server deletes the Send after this many days (1 to 31).
    pub deletion_days: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_access_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<Zeroizing<String>>,
    #[serde(default)]
    pub hide_email: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SendBodyData {
    Text {
        text: Zeroizing<String>,
        #[serde(default)]
        hidden: bool,
    },
    /// `data` is base64 in the message (a third larger, not four times as JSON numbers).
    File {
        name: String,
        #[serde(with = "base64_bytes")]
        data: Zeroizing<Vec<u8>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExportFormatData {
    Json,
    Csv,
    /// Bitwarden's password-protected JSON.
    EncryptedJson {
        password: Zeroizing<String>,
    },
}

impl ExportFormatData {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Csv => "csv",
            Self::EncryptedJson { .. } => "encrypted_json",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    pub data: Zeroizing<String>,
    pub items: u32,
}

/// Whether a file to import needs a password ([`Request::VaultImport`]): a KeePass
/// database or a password-protected Bitwarden export.
pub fn import_needs_password(data: &[u8]) -> bool {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Header {
        #[serde(default)]
        encrypted: bool,
        #[serde(default)]
        password_protected: bool,
    }
    // The KDBX signature.
    if data.starts_with(&[0x03, 0xd9, 0xa2, 0x9a]) {
        return true;
    }
    serde_json::from_slice::<Header>(data).is_ok_and(|h| h.encrypted && h.password_protected)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub items: u32,
    /// Folders created (existing ones with the same name are reused).
    pub folders: u32,
}

mod base64_bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize as _, Deserializer, Serializer};
    use zeroize::Zeroizing;

    pub fn serialize<S: Serializer>(data: &Zeroizing<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&Zeroizing::new(STANDARD.encode(data.as_slice())))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Zeroizing<Vec<u8>>, D::Error> {
        let text = Zeroizing::new(String::deserialize(d)?);
        STANDARD
            .decode(text.as_bytes())
            .map(Zeroizing::new)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldView {
    pub name: String,
    /// `None` for hidden fields.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanPromptResult {
    pub available: bool,
    /// SSH signatures from the caller's session are already authorized (`ssh.authorize`).
    #[serde(default)]
    pub ssh_authorized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    /// `false` when the server reported no changes since the last sync.
    pub ran: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasswordParams {
    pub length: u8,
    pub uppercase: bool,
    pub lowercase: bool,
    pub numbers: bool,
    pub special: bool,
    #[serde(default)]
    pub avoid_ambiguous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassphraseParams {
    pub words: u8,
    pub separator: String,
    #[serde(default)]
    pub capitalize: bool,
    #[serde(default)]
    pub include_number: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uris: Vec<String>,
    #[serde(default)]
    pub has_totp: bool,
    #[serde(default)]
    pub favorite: bool,
    /// In the trash (listed with `trash`).
    #[serde(default)]
    pub trashed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemListResult {
    pub items: Vec<ItemInfo>,
}

/// A single secret value.
#[derive(Debug, Serialize, Deserialize)]
pub struct ValueResult {
    pub value: Zeroizing<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshKeyEntry {
    pub id: String,
    pub name: String,
    /// `authorized_keys` format, without a comment.
    pub public_key: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshListResult {
    pub keys: Vec<SshKeyEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TotpResult {
    pub code: Zeroizing<String>,
    /// Seconds until the code changes.
    pub remaining: u32,
    pub period: u32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn request_wire_format() {
        let frame = RequestFrame {
            v: 1,
            id: 7,
            request: Request::Unlock {
                password: Zeroizing::new("pw".into()),
            },
            unlock: None,
        };
        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            json!({"v": 1, "id": 7, "method": "lock.unlock", "params": {"password": "pw"}})
        );
    }

    #[test]
    fn parameterless_request_round_trips() {
        let frame: RequestFrame =
            serde_json::from_value(json!({"v": 1, "id": 1, "method": "status"})).unwrap();
        assert!(matches!(frame.request, Request::Status));
        let frame: RequestFrame =
            serde_json::from_value(json!({"v": 1, "id": 2, "method": "sync", "params": {}}))
                .unwrap();
        assert!(matches!(frame.request, Request::Sync { force: false }));
    }

    #[test]
    fn unknown_method_is_an_error() {
        let parsed =
            serde_json::from_value::<RequestFrame>(json!({"v": 1, "id": 1, "method": "nope"}));
        assert!(parsed.is_err());
    }

    #[test]
    fn response_wire_format() {
        let ok = ResponseFrame {
            v: 1,
            id: 3,
            body: ResponseBody::Ok(json!({"ran": true})),
        };
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"v": 1, "id": 3, "result": {"ran": true}})
        );
        let err = ResponseFrame {
            v: 1,
            id: 4,
            body: ResponseBody::Err(ErrorBody {
                code: ErrorCode::TwoFactorRequired,
                message: "need code".into(),
                providers: vec![TwoFactorProvider::Email],
            }),
        };
        let value = serde_json::to_value(&err).unwrap();
        assert_eq!(
            value,
            json!({"v": 1, "id": 4, "error": {
                "code": "two_factor_required", "message": "need code", "providers": ["email"]
            }})
        );
        let back: ResponseFrame = serde_json::from_value(value).unwrap();
        assert!(
            matches!(back.body, ResponseBody::Err(e) if e.code == ErrorCode::TwoFactorRequired)
        );
    }
}

#[cfg(test)]
mod item_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn item_get_wire_format() {
        let frame: RequestFrame = serde_json::from_value(json!({
            "v": 1, "id": 9, "method": "item.get",
            "params": {"item": "GitHub", "field": "password"}
        }))
        .unwrap();
        assert!(matches!(
            frame.request,
            Request::ItemGet { folder: None, ref item, ref field } if item == "GitHub" && field == "password"
        ));
    }

    #[test]
    fn tells_which_imports_need_a_password() {
        assert!(import_needs_password(&[0x03, 0xd9, 0xa2, 0x9a, 0x67, 0xfb]));
        assert!(import_needs_password(
            br#"{"encrypted": true, "passwordProtected": true, "salt": "x"}"#
        ));
        assert!(!import_needs_password(
            br#"{"encrypted": false, "items": []}"#
        ));
        assert!(!import_needs_password(
            br#"{"encrypted": true, "passwordProtected": false}"#
        ));
        assert!(!import_needs_password(b"folder,favorite,type,name\n"));
    }
}
