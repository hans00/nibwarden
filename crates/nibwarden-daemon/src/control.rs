//! Control socket server (phase1.md §6).

use std::sync::Arc;

use nibwarden_core::{
    ExportFormat, ItemQuery, ItemRef, PassphraseOptions, PasswordOptions, Server, TwoFactor,
    TwoFactorProvider,
};
use nibwarden_ipc::{
    CanPromptResult, ErrorBody, ErrorCode, ExportFormatData, ExportResult, FieldView, FolderEntry,
    FolderListResult, FrameError, HelloResult, ImportResult, ItemInfo, ItemListResult, ItemView,
    LockState, PROTOCOL_VERSION, RegisterStartResult, Request, RequestFrame, ResponseBody,
    ResponseFrame, SavedResult, SendListResult, SshKeyEntry, SshListResult, SyncResult, TotpResult,
    UnlockWith, ValueResult,
    prompt::{Access, Action, Change, Scope, SendUse},
    read_frame, write_frame,
};
use nibwarden_platform::{
    peer::Peer,
    transport::{Listener, Stream},
};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::{
    approvals::{Approvals, Target, requester},
    daemon::Daemon,
    error::to_ipc,
    prompt_broker::Unlocked,
    send_data,
};

pub async fn serve(daemon: Arc<Daemon>, listener: Listener) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                tokio::spawn(handle_connection(daemon.clone(), stream));
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// A login that stopped at the two-factor step. Lives only as long as the connection.
struct PendingLogin {
    server: Server,
    email: String,
    password: Zeroizing<String>,
    /// The server wants this device confirmed, not a two-step code.
    new_device: bool,
}

struct Connection {
    daemon: Arc<Daemon>,
    peer: Peer,
    greeted: bool,
    pending_login: Option<PendingLogin>,
}

async fn handle_connection(daemon: Arc<Daemon>, mut stream: Stream) {
    let Some(peer) = Peer::identify_owner(&stream) else {
        return;
    };
    tracing::debug!(peer = peer.describe(), remote = ?peer.ssh_connection, "connected");

    let mut conn = Connection {
        daemon,
        peer,
        greeted: false,
        pending_login: None,
    };
    loop {
        let frame: RequestFrame = match read_frame(&mut stream).await {
            Ok(frame) => frame,
            Err(FrameError::Closed) => break,
            Err(e) => {
                // Includes malformed JSON and unknown methods; the id is unknown, so the
                // reply uses 0 and the connection is closed.
                let body = ErrorBody::new(ErrorCode::BadRequest, e.to_string());
                let _ = write_frame(&mut stream, &response(0, Err(body))).await;
                break;
            }
        };
        let id = frame.id;
        let fatal = frame.v != PROTOCOL_VERSION
            || (!conn.greeted && !matches!(frame.request, Request::Hello { .. }));
        let result = if frame.v != PROTOCOL_VERSION {
            Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "unsupported protocol version {}; daemon speaks {PROTOCOL_VERSION}",
                    frame.v
                ),
            ))
        } else if fatal {
            Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "first request must be hello",
            ))
        } else {
            let method = frame.request.method();
            tracing::debug!(id, method, "request");
            conn.dispatch(frame.request, frame.unlock).await
        };
        if write_frame(&mut stream, &response(id, result))
            .await
            .is_err()
            || fatal
        {
            break;
        }
    }
    tracing::debug!(peer = conn.peer.describe(), "disconnected");
}

fn response(id: u64, result: Result<Value, ErrorBody>) -> ResponseFrame {
    ResponseFrame {
        v: PROTOCOL_VERSION,
        id,
        body: match result {
            Ok(value) => ResponseBody::Ok(value),
            Err(e) => ResponseBody::Err(e),
        },
    }
}

fn to_value(value: impl serde::Serialize) -> Result<Value, ErrorBody> {
    serde_json::to_value(value).map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))
}

impl Connection {
    async fn dispatch(
        &mut self,
        request: Request,
        unlock: Option<UnlockWith>,
    ) -> Result<Value, ErrorBody> {
        if let Request::Hello { client, version } = &request {
            tracing::debug!(client, version, "hello");
            self.greeted = true;
            return to_value(HelloResult {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            });
        }
        // Everything is open to the owning user (checked at connect); reading a secret
        // additionally needs the user's confirmation (phase2.md §4.2), handled below.
        let allowed_by_unlock = match unlock {
            Some(how) if needs_vault(&request) => self.unlock_first(how, &request).await?,
            _ => None,
        };

        let daemon = self.daemon.clone();
        match request {
            Request::Hello { .. } => unreachable!("handled above"),
            Request::Status => to_value(daemon.status().await),
            Request::Login {
                server,
                email,
                password,
            } => {
                let server = parse_server(&server)?;
                // The master key is salted with the address as the official clients spell it.
                let email = nibwarden_core::normalize_email(&email);
                let result = daemon.login(&server, &email, &password, None).await;
                if let Err(e) = &result
                    && e.code == ErrorCode::TwoFactorRequired
                {
                    self.pending_login = Some(PendingLogin {
                        server,
                        email,
                        password,
                        new_device: e
                            .providers
                            .contains(&nibwarden_ipc::TwoFactorProvider::NewDevice),
                    });
                }
                to_value(result?)
            }
            Request::SendEmailCode => {
                let pending = self.pending()?;
                let sent = if pending.new_device {
                    nibwarden_core::Account::send_new_device_code(
                        &pending.server,
                        &pending.email,
                        &pending.password,
                    )
                    .await
                } else {
                    nibwarden_core::Account::send_two_factor_email(
                        &pending.server,
                        &pending.email,
                        &pending.password,
                    )
                    .await
                };
                sent.map_err(to_ipc)?;
                Ok(Value::Null)
            }
            Request::Login2fa { provider, token } => {
                let pending = self.pending()?;
                let two_factor = TwoFactor {
                    provider: match provider {
                        nibwarden_ipc::TwoFactorProvider::Authenticator => {
                            TwoFactorProvider::Authenticator
                        }
                        nibwarden_ipc::TwoFactorProvider::Email => TwoFactorProvider::Email,
                        nibwarden_ipc::TwoFactorProvider::NewDevice => TwoFactorProvider::NewDevice,
                    },
                    token: token.to_string(),
                };
                let status = daemon
                    .login(
                        &pending.server,
                        &pending.email,
                        &pending.password,
                        Some(two_factor),
                    )
                    .await?;
                self.pending_login = None;
                to_value(status)
            }
            Request::RegisterStart {
                server,
                email,
                name,
            } => {
                let server = parse_server(&server)?;
                let token = nibwarden_core::start_registration(&server, &email, &name)
                    .await
                    .map_err(to_ipc)?;
                to_value(RegisterStartResult { token })
            }
            Request::RegisterFinish {
                server,
                email,
                token,
                password,
                hint,
            } => {
                let server = parse_server(&server)?;
                nibwarden_core::finish_registration(
                    &server,
                    &email,
                    &token,
                    &password,
                    hint.as_deref(),
                )
                .await
                .map_err(to_ipc)?;
                Ok(Value::Null)
            }
            Request::Logout => daemon.logout().await.map(|()| Value::Null),
            Request::Unlock { password } => daemon.unlock(&password).await.map(|()| Value::Null),
            Request::UnlockPrompt => daemon
                .prompts
                .unlock(&daemon, &self.peer)
                .await
                .map(|()| Value::Null),
            Request::ItemView { folder, item } => {
                let query = ItemQuery {
                    folder: folder.as_deref(),
                    item: &item,
                };
                let d = daemon.item_details(query).await?;
                to_value(ItemView {
                    id: d.item.id,
                    name: d.item.name,
                    kind: d.kind.to_owned(),
                    folder: d.folder,
                    username: d.username,
                    has_password: d.has_password,
                    has_totp: d.has_totp,
                    has_notes: d.has_notes,
                    uris: d.uris,
                    fields: d
                        .fields
                        .into_iter()
                        .map(|f| FieldView {
                            name: f.name,
                            value: f.value,
                        })
                        .collect(),
                    reprompt: d.item.reprompt,
                    values: d.values,
                    favorite: d.favorite,
                    trashed: d.trashed,
                    created: d.created,
                    revised: d.revised,
                    password_revised: d.password_revised,
                    password_history: d.password_history,
                })
            }
            Request::SshAuthorize { password } => {
                let unlocked = if daemon.lock_state().await == LockState::Locked {
                    daemon.unlock(&password).await.map(|()| true)?
                } else {
                    daemon.verify_password(&password).await?
                };
                if !unlocked {
                    return Err(ErrorBody::new(ErrorCode::Denied, "wrong master password"));
                }
                daemon.approvals.authorize_terminal(&self.peer);
                Ok(Value::Null)
            }
            Request::ReloadConfig => {
                daemon.reload_config();
                Ok(Value::Null)
            }
            Request::CanPrompt => to_value(CanPromptResult {
                available: daemon.prompts.can_prompt(&self.peer),
                ssh_authorized: daemon.approvals.ssh_authorized(&self.peer),
            }),
            Request::Lock => daemon.lock().await.map(|()| Value::Null),
            Request::Sync { force } => to_value(SyncResult {
                ran: daemon.sync(force).await?,
            }),
            Request::ItemList {
                search,
                folder,
                trash,
            } => {
                let search = search.map(|s| s.to_lowercase());
                let items = daemon
                    .list_items(trash)
                    .await?
                    .into_iter()
                    .filter(|i| {
                        folder.as_ref().is_none_or(|f| {
                            i.folder
                                .as_ref()
                                .is_some_and(|name| name.eq_ignore_ascii_case(f))
                        })
                    })
                    .filter(|i| {
                        search.as_ref().is_none_or(|q| {
                            i.name.to_lowercase().contains(q)
                                || i.subtitle.to_lowercase().contains(q)
                                || i.uris.iter().any(|u| u.to_lowercase().contains(q))
                        })
                    })
                    .map(|i| ItemInfo {
                        id: i.id,
                        name: i.name,
                        kind: i.kind.to_owned(),
                        subtitle: i.subtitle,
                        folder: i.folder,
                        uris: i.uris,
                        has_totp: i.has_totp,
                        favorite: i.favorite,
                        trashed: trash,
                    })
                    .collect();
                to_value(ItemListResult { items })
            }
            Request::ItemGet {
                folder,
                item,
                field,
            } => {
                let query = ItemQuery {
                    folder: folder.as_deref(),
                    item: &item,
                };
                let secret = daemon.read_field(query, &field).await?;
                self.allow_secret(allowed_by_unlock, &secret.item, &field)
                    .await?;
                to_value(ValueResult {
                    value: secret.value,
                })
            }
            Request::ItemTotp { folder, item } => {
                let query = ItemQuery {
                    folder: folder.as_deref(),
                    item: &item,
                };
                let (totp, from) = daemon.totp(query).await?;
                self.allow_secret(allowed_by_unlock, &from, "totp").await?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let period = totp.period.max(1);
                to_value(TotpResult {
                    code: totp.code,
                    remaining: period - (now % u64::from(period)) as u32,
                    period,
                })
            }
            Request::GeneratePassword(p) => {
                let value = nibwarden_core::generate_password(&PasswordOptions {
                    length: p.length,
                    uppercase: p.uppercase,
                    lowercase: p.lowercase,
                    numbers: p.numbers,
                    special: p.special,
                    avoid_ambiguous: p.avoid_ambiguous,
                })
                .map_err(to_ipc)?;
                to_value(ValueResult { value })
            }
            Request::GeneratePassphrase(p) => {
                let value = nibwarden_core::generate_passphrase(&PassphraseOptions {
                    words: p.words,
                    separator: p.separator,
                    capitalize: p.capitalize,
                    include_number: p.include_number,
                })
                .map_err(to_ipc)?;
                to_value(ValueResult { value })
            }
            Request::SshList => {
                let keys = daemon
                    .ssh_keys()
                    .await?
                    .into_iter()
                    .map(|k| SshKeyEntry {
                        id: k.id,
                        name: k.name,
                        public_key: k.public_key,
                        fingerprint: k.fingerprint,
                    })
                    .collect();
                to_value(SshListResult { keys })
            }
            Request::ItemContent { id } => {
                let account = daemon.in_use().await?;
                let (content, item) = account.item_content(&id).await.map_err(to_ipc)?;
                self.allow_secret(allowed_by_unlock, &item, "all").await?;
                to_value(crate::item_data::to_ipc(content))
            }
            Request::ItemSave { item } => {
                let account = daemon.in_use().await?;
                let (name, change) = match &item.id {
                    Some(id) => (
                        account.item_ref(id).await.map_err(to_ipc)?.0.name,
                        Change::Edit,
                    ),
                    None => (item.name.clone(), Change::Create),
                };
                self.allow_change(&name, change).await?;
                let id = account
                    .save_item(&crate::item_data::from_ipc(item))
                    .await
                    .map_err(to_ipc)?;
                tracing::info!(id, change = change.as_str(), "item saved");
                to_value(SavedResult { id })
            }
            Request::ItemFavorite { id, favorite } => {
                let account = daemon.in_use().await?;
                let (item, _) = account.item_ref(&id).await.map_err(to_ipc)?;
                self.allow_change(&item.name, Change::Favorite).await?;
                account.set_favorite(&id, favorite).await.map_err(to_ipc)?;
                Ok(Value::Null)
            }
            Request::ItemTrash { id } => {
                let account = daemon.in_use().await?;
                let (item, _) = account.item_ref(&id).await.map_err(to_ipc)?;
                self.allow_change(&item.name, Change::Trash).await?;
                account.trash_item(&id).await.map_err(to_ipc)?;
                tracing::info!(id, "item moved to the trash");
                Ok(Value::Null)
            }
            Request::ItemRestore { id } => {
                let account = daemon.in_use().await?;
                let (item, _) = account.item_ref(&id).await.map_err(to_ipc)?;
                self.allow_change(&item.name, Change::Restore).await?;
                account.restore_item(&id).await.map_err(to_ipc)?;
                tracing::info!(id, "item restored");
                Ok(Value::Null)
            }
            Request::ItemDelete { id } => {
                let account = daemon.in_use().await?;
                let (item, _) = account.item_ref(&id).await.map_err(to_ipc)?;
                self.allow_change(&item.name, Change::Delete).await?;
                account.delete_item(&id).await.map_err(to_ipc)?;
                tracing::info!(id, "item deleted");
                Ok(Value::Null)
            }
            Request::FolderList => {
                let folders = daemon
                    .in_use()
                    .await?
                    .folders()
                    .await
                    .map_err(to_ipc)?
                    .into_iter()
                    .map(|f| FolderEntry {
                        id: f.id,
                        name: f.name,
                    })
                    .collect();
                to_value(FolderListResult { folders })
            }
            Request::SendList => {
                let sends = daemon.in_use().await?.list_sends().await.map_err(to_ipc)?;
                to_value(SendListResult {
                    sends: sends.into_iter().map(send_data::entry).collect(),
                })
            }
            Request::SendGet { id } => {
                let account = daemon.in_use().await?;
                let send = account.send_info(&id).await.map_err(to_ipc)?;
                self.allow_send(&send.name, SendUse::Read).await?;
                to_value(send_data::secret(
                    account.send_secret(&id).await.map_err(to_ipc)?,
                ))
            }
            Request::SendCreate { send } => {
                let account = daemon.in_use().await?;
                self.allow_send(&send.name, SendUse::Create).await?;
                let id = account
                    .create_send(send_data::new_send(send))
                    .await
                    .map_err(to_ipc)?;
                tracing::info!(id, "send created");
                to_value(SavedResult { id })
            }
            Request::SendDelete { id } => {
                let account = daemon.in_use().await?;
                let send = account.send_info(&id).await.map_err(to_ipc)?;
                self.allow_send(&send.name, SendUse::Delete).await?;
                account.delete_send(&id).await.map_err(to_ipc)?;
                tracing::info!(id, "send deleted");
                Ok(Value::Null)
            }
            Request::VaultExport {
                format,
                master_password,
            } => {
                let account = daemon.in_use().await?;
                // Asked of everyone, our own window too: the file holds every secret.
                if !daemon.verify_password(&master_password).await? {
                    return Err(ErrorBody::new(ErrorCode::Denied, "wrong master password"));
                }
                let name = format.as_str();
                let action = Action::Export {
                    format: name.to_owned(),
                };
                self.allow_once(action, "vault", name, "exporting the vault")
                    .await?;
                let format = match format {
                    ExportFormatData::Json => ExportFormat::Json,
                    ExportFormatData::Csv => ExportFormat::Csv,
                    ExportFormatData::EncryptedJson { password } => {
                        ExportFormat::EncryptedJson(password)
                    }
                };
                let export = account.export_vault(format).await.map_err(to_ipc)?;
                tracing::info!(format = name, items = export.items, "vault exported");
                to_value(ExportResult {
                    data: export.data,
                    items: export.items as u32,
                })
            }
            Request::VaultImport {
                name,
                data,
                password,
            } => {
                let account = daemon.in_use().await?;
                let file = nibwarden_core::read_import(data.to_vec(), password).map_err(to_ipc)?;
                let items = file.items().map(|n| n as u32);
                let action = Action::Import {
                    source: name.clone(),
                    items,
                };
                self.allow_once(action, &name, "import", "importing into the vault")
                    .await?;
                let imported = account.import(file).await.map_err(to_ipc)?;
                tracing::info!(
                    items = imported.items,
                    folders = imported.folders,
                    "vault imported"
                );
                to_value(ImportResult {
                    items: imported.items as u32,
                    folders: imported.folders as u32,
                })
            }
            Request::BrowserMessage { message } => {
                crate::browser::handle(&daemon, &self.peer, message).await
            }
        }
    }

    /// Lets one change to the vault through. Our own window acts for the user; any other
    /// program is asked about every time, never remembered (phase2.md §12.10).
    async fn allow_change(&self, item: &str, change: Change) -> Result<(), ErrorBody> {
        let action = Action::ChangeItem {
            item: item.to_owned(),
            change,
        };
        self.allow_once(action, item, change.as_str(), "changing the vault")
            .await
    }

    /// Lets one use of a Send through, the same way: reading its link hands out its
    /// content, so that is asked about too.
    async fn allow_send(&self, send: &str, what: SendUse) -> Result<(), ErrorBody> {
        let action = Action::Send {
            send: send.to_owned(),
            what,
        };
        let doing = match what {
            SendUse::Read => "reading a Send",
            SendUse::Create | SendUse::Delete => "changing your Sends",
        };
        self.allow_once(action, send, what.as_str(), doing).await
    }

    /// Asks the user about `action` (on `target`, for the log) unless it comes from our
    /// own window; `doing` names it in the error when no window can ask.
    async fn allow_once(
        &self,
        action: Action,
        target: &str,
        change: &str,
        doing: &str,
    ) -> Result<(), ErrorBody> {
        if self.daemon.prompts.is_own_gui(&self.peer) {
            return Ok(());
        }
        let access = Access {
            requester: requester(&self.peer),
            action,
        };
        let application = Approvals::application(&self.peer);
        match self
            .daemon
            .prompts
            .approve(&self.peer, access, application, Scope::Request)
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.code == ErrorCode::Unavailable => Err(ErrorBody::new(
                ErrorCode::Denied,
                format!("{doing} needs your confirmation, but {}", e.message),
            )),
            Err(e) => {
                tracing::info!(target, change, reason = e.message, "change refused");
                Err(e)
            }
        }
    }

    /// Unlocks a locked vault the way the request asked (phase2.md §5.3).
    /// `Some(all_applications)` if that unlock also allows the request: the master
    /// password was just entered for it, in the terminal or in a window that showed it.
    async fn unlock_first(
        &self,
        how: UnlockWith,
        request: &Request,
    ) -> Result<Option<bool>, ErrorBody> {
        let daemon = &self.daemon;
        if daemon.lock_state().await != LockState::Locked {
            return Ok(None);
        }
        match how {
            UnlockWith::Password { password } => {
                daemon.unlock(&password).await?;
                Ok(Some(false))
            }
            UnlockWith::Window => match secret_access(&self.peer, request) {
                Some(access) => {
                    let application = Approvals::application(&self.peer);
                    let scope = daemon.approvals.scope();
                    match daemon
                        .prompts
                        .unlock_for(daemon, &self.peer, access, application, scope)
                        .await?
                    {
                        Unlocked::AndAllowed { all_applications } => Ok(Some(all_applications)),
                        Unlocked::Already => Ok(None),
                    }
                }
                None => {
                    daemon.prompts.unlock(daemon, &self.peer).await?;
                    Ok(None)
                }
            },
        }
    }

    /// Lets one secret out: allowed by the unlock just done for it, or by approval.
    async fn allow_secret(
        &self,
        allowed_by_unlock: Option<bool>,
        item: &ItemRef,
        field: &str,
    ) -> Result<(), ErrorBody> {
        let target = Target::Item {
            id: item.id.clone(),
            field: field.to_lowercase(),
        };
        if let Some(all_applications) = allowed_by_unlock {
            self.daemon
                .approvals
                .remember(&self.peer, target, all_applications);
            return Ok(());
        }
        if !item.reprompt && self.daemon.prompts.is_own_gui(&self.peer) {
            return Ok(());
        }
        let access = Access {
            requester: requester(&self.peer),
            action: Action::ReadSecret {
                item: item.name.clone(),
                field: field.to_owned(),
            },
        };
        self.daemon
            .approvals
            .approve(&self.daemon, &self.peer, access, target, item.reprompt)
            .await
            .inspect_err(|e| {
                tracing::info!(item = item.id, field, reason = e.message, "secret withheld")
            })
    }

    fn pending(&self) -> Result<&PendingLogin, ErrorBody> {
        self.pending_login.as_ref().ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::BadRequest,
                "no login waiting for a two-step code on this connection",
            )
        })
    }
}

/// Requests that read the decrypted vault, so a locked vault must be unlocked first.
/// The SDK's HTTP client is HTTPS-only in release builds; say so up front instead of
/// failing with an opaque request-builder error.
fn parse_server(input: &str) -> Result<Server, ErrorBody> {
    let server = Server::parse(input);
    if !cfg!(debug_assertions) && !server.is_https() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            "the server URL must start with https://",
        ));
    }
    Ok(server)
}

fn needs_vault(request: &Request) -> bool {
    matches!(
        request,
        Request::ItemList { .. }
            | Request::ItemGet { .. }
            | Request::ItemTotp { .. }
            | Request::ItemView { .. }
            | Request::SshList
            | Request::ItemContent { .. }
            | Request::ItemSave { .. }
            | Request::ItemFavorite { .. }
            | Request::ItemTrash { .. }
            | Request::ItemRestore { .. }
            | Request::ItemDelete { .. }
            | Request::FolderList
            | Request::SendList
            | Request::SendGet { .. }
            | Request::SendCreate { .. }
            | Request::SendDelete { .. }
            | Request::VaultExport { .. }
            | Request::VaultImport { .. }
    )
}

/// The secret a request reads, as the user asked for it (the item is resolved only after
/// unlocking).
fn secret_access(peer: &Peer, request: &Request) -> Option<Access> {
    let (item, field) = match request {
        Request::ItemGet { item, field, .. } => (item.clone(), field.clone()),
        Request::ItemTotp { item, .. } => (item.clone(), "totp".to_owned()),
        _ => return None,
    };
    Some(Access {
        requester: requester(peer),
        action: Action::ReadSecret { item, field },
    })
}
