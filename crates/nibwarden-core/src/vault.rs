//! Read access to the decrypted vault: listing, item lookup, single-field reads, TOTP.
//! Nothing here hands out a whole decrypted item; callers ask for one field at a time.

use std::collections::HashMap;

use bitwarden_vault::{
    CipherCreateRequest, CipherListView, CipherListViewType, CipherRepromptType, CipherType,
    CipherView, CipherViewType, FieldType, FieldView, FolderAddEditRequest, FolderId, LoginUriView,
    LoginView, SshKeyView, generate_totp,
};
use zeroize::Zeroizing;

use crate::{Account, Error, error::SdkContext as _};

/// Non-sensitive listing data; never contains secrets.
#[derive(Debug, Clone)]
pub struct ItemSummary {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    /// What the official clients show under the name (username, card brand + last 4, ...).
    pub subtitle: String,
    pub folder: Option<String>,
    pub uris: Vec<String>,
    pub has_totp: bool,
    pub favorite: bool,
}

/// Selects one item: by id, or by name (exact first, then case-insensitive), optionally
/// restricted to a folder given by name or id.
#[derive(Debug, Clone, Copy)]
pub struct ItemQuery<'a> {
    pub folder: Option<&'a str>,
    pub item: &'a str,
}

#[derive(Debug)]
pub struct Totp {
    pub code: Zeroizing<String>,
    /// Seconds per code.
    pub period: u32,
}

/// The item a secret came from, for approval prompts and policy (phase2.md §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRef {
    pub id: String,
    pub name: String,
    /// The item asks for the master password again before its secrets are used.
    pub reprompt: bool,
}

impl ItemRef {
    pub(crate) fn of(view: &CipherView) -> Self {
        Self {
            id: view.id.map(|id| id.to_string()).unwrap_or_default(),
            name: view.name.clone(),
            reprompt: view.reprompt != CipherRepromptType::None,
        }
    }
}

/// An item without its secrets, for a detail view: which secrets exist is said, their
/// values are read one at a time with [`Account::read_field`].
#[derive(Debug, Clone)]
pub struct ItemDetails {
    pub item: ItemRef,
    pub kind: &'static str,
    pub folder: Option<String>,
    pub username: Option<String>,
    pub has_password: bool,
    pub has_totp: bool,
    pub has_notes: bool,
    pub uris: Vec<String>,
    /// Custom fields; hidden ones without their value.
    pub fields: Vec<FieldInfo>,
    /// Type-specific values of cards, identities and SSH keys, by key in form order
    /// ([`crate::KINDS`]); secret ones (`None`) are read with [`Account::read_field`].
    /// Only the keys the item has.
    pub values: Vec<(String, Option<String>)>,
    pub favorite: bool,
    /// In the trash.
    pub trashed: bool,
    /// When the item was created and last changed, and its password last changed (RFC 3339).
    pub created: String,
    pub revised: String,
    pub password_revised: Option<String>,
    /// When each earlier password was replaced, newest first; the passwords themselves
    /// are read with [`Account::read_field`] as `password_history_<index>`.
    pub password_history: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    /// `None` for hidden fields: read them like any other secret.
    pub value: Option<String>,
}

/// One field value and where it came from.
pub struct Secret {
    pub value: Zeroizing<String>,
    pub item: ItemRef,
}

/// An SSH key item, without its private key.
#[derive(Debug, Clone)]
pub struct SshKeyInfo {
    pub id: String,
    pub name: String,
    /// `authorized_keys` format, e.g. `ssh-ed25519 AAAA…`.
    pub public_key: String,
    /// `SHA256:…`
    pub fingerprint: String,
    /// The item asks for the master password again before the key is used.
    pub reprompt: bool,
}

/// Test-data helper input for [`Account::create_login`].
#[derive(Debug, Default)]
pub struct NewLogin<'a> {
    pub name: &'a str,
    pub username: Option<&'a str>,
    pub password: Option<&'a str>,
    pub totp: Option<&'a str>,
    pub uri: Option<&'a str>,
    pub notes: Option<&'a str>,
    /// Stored as hidden custom fields.
    pub fields: &'a [(&'a str, &'a str)],
    pub folder: Option<&'a str>,
    /// Ask for the master password again before use.
    pub reprompt: bool,
}

impl Account {
    fn ensure_unlocked(&self) -> Result<(), Error> {
        if self.is_unlocked() {
            Ok(())
        } else {
            Err(Error::Locked)
        }
    }

    async fn folder_names(&self) -> Result<HashMap<FolderId, String>, Error> {
        let folders = self
            .client
            .vault()
            .folders()
            .list()
            .await
            .sdk("list folders")?;
        Ok(folders
            .into_iter()
            .filter_map(|f| Some((f.id?, f.name)))
            .collect())
    }

    /// Live items (or only the trashed ones), decrypted to list views.
    async fn list_views(&self, trash: bool) -> Result<Vec<CipherListView>, Error> {
        self.ensure_unlocked()?;
        let result = self
            .client
            .vault()
            .ciphers()
            .list()
            .await
            .sdk("list items")?;
        if !result.failures.is_empty() {
            tracing::warn!(count = result.failures.len(), "items failed to decrypt");
        }
        Ok(result
            .successes
            .into_iter()
            .filter(|c| c.deleted_date.is_some() == trash && c.id.is_some())
            .collect())
    }

    /// Live items, or with `trash` the ones in the trash.
    pub async fn list_items(&self, trash: bool) -> Result<Vec<ItemSummary>, Error> {
        let views = self.list_views(trash).await?;
        let folders = self.folder_names().await?;
        let mut items: Vec<ItemSummary> = views
            .into_iter()
            .map(|c| {
                let (uris, has_totp) = match &c.r#type {
                    CipherListViewType::Login(login) => (
                        login
                            .uris
                            .iter()
                            .flatten()
                            .filter_map(|u| u.uri.clone())
                            .collect(),
                        login.totp.is_some(),
                    ),
                    _ => (Vec::new(), false),
                };
                ItemSummary {
                    id: c.id.map(|id| id.to_string()).unwrap_or_default(),
                    kind: kind_name(&c.r#type),
                    folder: c.folder_id.and_then(|id| folders.get(&id).cloned()),
                    name: c.name,
                    subtitle: c.subtitle,
                    uris,
                    has_totp,
                    favorite: c.favorite,
                }
            })
            .collect();
        items.sort_by_key(|a| a.name.to_lowercase());
        Ok(items)
    }

    /// Resolves a query to exactly one item and decrypts it.
    async fn find_item(&self, query: ItemQuery<'_>) -> Result<CipherView, Error> {
        let mut views = self.list_views(false).await?;
        if let Some(folder) = query.folder {
            let folders = self.folder_names().await?;
            let wanted: Vec<FolderId> = folders
                .iter()
                .filter(|(id, name)| {
                    id.to_string() == folder || name.eq_ignore_ascii_case(folder) || *name == folder
                })
                .map(|(id, _)| *id)
                .collect();
            if wanted.is_empty() {
                return Err(Error::NotFound(format!("no folder named {folder:?}")));
            }
            views.retain(|c| c.folder_id.is_some_and(|id| wanted.contains(&id)));
        }

        let by_id: Vec<&CipherListView> = views
            .iter()
            .filter(|c| c.id.is_some_and(|id| id.to_string() == query.item))
            .collect();
        let exact: Vec<&CipherListView> = views.iter().filter(|c| c.name == query.item).collect();
        let candidates = if !by_id.is_empty() {
            by_id
        } else if !exact.is_empty() {
            exact
        } else {
            views
                .iter()
                .filter(|c| c.name.to_lowercase() == query.item.to_lowercase())
                .collect()
        };
        let id = match candidates.as_slice() {
            [] => return Err(Error::NotFound(format!("no item named {:?}", query.item))),
            [one] => one.id.expect("filtered above"),
            many => {
                let listed: Vec<String> = many
                    .iter()
                    .take(5)
                    .map(|c| format!("{} ({})", c.name, c.id.expect("filtered above")))
                    .collect();
                return Err(Error::Ambiguous(format!(
                    "{} items match {:?}: {}; use the id or a folder",
                    many.len(),
                    query.item,
                    listed.join(", ")
                )));
            }
        };
        self.client
            .vault()
            .ciphers()
            .get(id)
            .await
            .sdk("decrypt item")
    }

    /// Returns one field of one item. `field` is a built-in name (see [`builtin_field`]) or
    /// the name of a custom field.
    pub async fn read_field(&self, query: ItemQuery<'_>, field: &str) -> Result<Secret, Error> {
        let view = self.find_item(query).await?;
        let item = ItemRef::of(&view);
        if field.eq_ignore_ascii_case("totp") {
            let value = totp_of(&view)?.code;
            return Ok(Secret { value, item });
        }
        let value = match builtin_field(&view, field)? {
            Some(value) => Some(value),
            None => custom_field(&view, field)?,
        };
        let value = value
            .ok_or_else(|| Error::NotFound(format!("{:?} has no field {field:?}", view.name)))?;
        Ok(Secret { value, item })
    }

    /// The item at `query` without secret values (see [`ItemDetails`]).
    pub async fn item_details(&self, query: ItemQuery<'_>) -> Result<ItemDetails, Error> {
        let view = self.find_item(query).await?;
        self.details_of(view).await
    }

    /// Like [`Self::item_details`], for an item in the trash too.
    pub async fn trashed_item_details(&self, id: &str) -> Result<ItemDetails, Error> {
        let view = self.item_by_id(id).await?;
        self.details_of(view).await
    }

    async fn details_of(&self, view: CipherView) -> Result<ItemDetails, Error> {
        let folder = match view.folder_id {
            Some(id) => self.folder_names().await?.get(&id).cloned(),
            None => None,
        };
        let login = view.login.as_ref();
        let fields = view
            .fields
            .iter()
            .flatten()
            .filter(|f| f.r#type != FieldType::Linked)
            .map(|f| FieldInfo {
                name: f.name.clone().unwrap_or_default(),
                value: (f.r#type != FieldType::Hidden).then(|| f.value.clone().unwrap_or_default()),
            })
            .collect();
        Ok(ItemDetails {
            item: ItemRef::of(&view),
            kind: match view.r#type {
                CipherType::Login => "login",
                CipherType::SecureNote => "note",
                CipherType::Card => "card",
                CipherType::Identity => "identity",
                CipherType::SshKey => "ssh_key",
                _ => "other",
            },
            folder,
            username: login.and_then(|l| l.username.clone()),
            has_password: login.is_some_and(|l| l.password.is_some()),
            has_totp: login.is_some_and(|l| l.totp.is_some()),
            has_notes: view.notes.as_deref().is_some_and(|n| !n.is_empty()),
            uris: login
                .and_then(|l| l.uris.as_ref())
                .map(|uris| uris.iter().filter_map(|u| u.uri.clone()).collect())
                .unwrap_or_default(),
            fields,
            values: crate::edit::values_for_view(&view),
            favorite: view.favorite,
            trashed: view.deleted_date.is_some(),
            created: view.creation_date.to_rfc3339(),
            revised: view.revision_date.to_rfc3339(),
            password_revised: login
                .and_then(|l| l.password_revision_date)
                .map(|d| d.to_rfc3339()),
            password_history: view
                .password_history
                .iter()
                .flatten()
                .map(|h| h.last_used_date.to_rfc3339())
                .collect(),
        })
    }

    pub async fn totp(&self, query: ItemQuery<'_>) -> Result<(Totp, ItemRef), Error> {
        let view = self.find_item(query).await?;
        Ok((totp_of(&view)?, ItemRef::of(&view)))
    }

    /// Every SSH key item, in vault order. Items that fail to decrypt are skipped.
    pub async fn ssh_keys(&self) -> Result<Vec<SshKeyInfo>, Error> {
        let views = self.list_views(false).await?;
        let mut keys = Vec::new();
        for c in views
            .iter()
            .filter(|c| matches!(c.r#type, CipherListViewType::SshKey))
        {
            let id = c.id.expect("filtered in list_views");
            let view = match self.client.vault().ciphers().get(id).await {
                Ok(view) => view,
                Err(e) => {
                    tracing::warn!(%id, error = %e, "SSH key item failed to decrypt");
                    continue;
                }
            };
            if let Some(key) = view.ssh_key {
                // The private key is not needed here; wipe our copy right away.
                drop(Zeroizing::new(key.private_key));
                keys.push(SshKeyInfo {
                    id: id.to_string(),
                    name: view.name,
                    public_key: key.public_key,
                    fingerprint: key.fingerprint,
                    reprompt: view.reprompt != CipherRepromptType::None,
                });
            }
        }
        Ok(keys)
    }

    /// The OpenSSH-format private key of the SSH key item `id`, for signing inside the
    /// daemon. Unlike field reads this ignores the organisation's "hide passwords"
    /// setting: signing does not reveal the key, and the official desktop agent signs too.
    pub async fn ssh_private_key(&self, id: &str) -> Result<Zeroizing<String>, Error> {
        self.ensure_unlocked()?;
        let view = self
            .client
            .vault()
            .ciphers()
            .get(id.parse().sdk("parse item id")?)
            .await
            .sdk("decrypt item")?;
        view.ssh_key
            .map(|k| Zeroizing::new(k.private_key))
            .ok_or_else(|| Error::NotFound(format!("{:?} is not an SSH key", view.name)))
    }

    /// Stores an SSH private key (OpenSSH or PKCS#8 PEM, unencrypted) as a new SSH key
    /// item; returns its id. (Test-data helper.)
    pub async fn import_ssh_key(&self, name: &str, pem: &str) -> Result<String, Error> {
        let key = bitwarden_ssh::import::import_key(pem.to_owned(), None)
            .map_err(|e| Error::Invalid(format!("cannot import SSH key: {e}")))?;
        self.create_item(
            name,
            None,
            None,
            CipherViewType::SshKey(SshKeyView {
                private_key: key.private_key,
                public_key: key.public_key,
                fingerprint: key.fingerprint,
            }),
            Vec::new(),
            false,
        )
        .await
    }

    /// Creates a folder; returns its id. (Test-data helper.)
    pub async fn create_folder(&self, name: &str) -> Result<String, Error> {
        self.ensure_unlocked()?;
        let folder = self
            .client
            .vault()
            .folders()
            .create(FolderAddEditRequest {
                name: name.to_owned(),
            })
            .await
            .sdk("create folder")?;
        Ok(folder.id.map(|id| id.to_string()).unwrap_or_default())
    }

    /// Creates a login item in the personal vault; returns its id. (Test-data helper.)
    pub async fn create_login(&self, new: &NewLogin<'_>) -> Result<String, Error> {
        let login = CipherViewType::Login(LoginView {
            username: new.username.map(str::to_owned),
            password: new.password.map(str::to_owned),
            password_revision_date: None,
            uris: new.uri.map(|uri| {
                vec![LoginUriView {
                    uri: Some(uri.to_owned()),
                    r#match: None,
                    uri_checksum: None,
                }]
            }),
            totp: new.totp.map(str::to_owned),
            autofill_on_page_load: None,
            fido2_credentials: None,
        });
        let fields = new
            .fields
            .iter()
            .map(|(name, value)| FieldView {
                name: Some((*name).to_owned()),
                value: Some((*value).to_owned()),
                r#type: FieldType::Hidden,
                linked_id: None,
            })
            .collect();
        self.create_item(new.name, new.notes, new.folder, login, fields, new.reprompt)
            .await
    }

    async fn create_item(
        &self,
        name: &str,
        notes: Option<&str>,
        folder: Option<&str>,
        r#type: CipherViewType,
        fields: Vec<FieldView>,
        reprompt: bool,
    ) -> Result<String, Error> {
        self.ensure_unlocked()?;
        let folder_id = match folder {
            Some(folder) => Some(folder.parse().sdk("parse folder id")?),
            None => None,
        };
        let view = self
            .client
            .vault()
            .ciphers()
            .create(CipherCreateRequest {
                organization_id: None,
                collection_ids: Vec::new(),
                folder_id,
                name: name.to_owned(),
                notes: notes.map(str::to_owned),
                favorite: false,
                reprompt: if reprompt {
                    CipherRepromptType::Password
                } else {
                    CipherRepromptType::None
                },
                r#type,
                fields,
                archived_date: None,
            })
            .await
            .sdk("create item")?;
        Ok(view.id.map(|id| id.to_string()).unwrap_or_default())
    }
}

/// Organisations can hide passwords (and hidden fields, TOTP) from members.
fn require_view_password(view: &CipherView) -> Result<(), Error> {
    if view.view_password {
        Ok(())
    } else {
        Err(Error::Denied(format!(
            "the organisation does not allow viewing secrets of {:?}",
            view.name
        )))
    }
}

fn totp_of(view: &CipherView) -> Result<Totp, Error> {
    require_view_password(view)?;
    let key = view
        .login
        .as_ref()
        .and_then(|l| l.totp.clone())
        .ok_or_else(|| Error::NotFound(format!("{:?} has no TOTP", view.name)))?;
    let response = generate_totp(key, None).sdk("generate TOTP")?;
    Ok(Totp {
        code: Zeroizing::new(response.code),
        period: response.period,
    })
}

/// `password_history_0` is the password replaced last.
const HISTORY_PREFIX: &str = "password_history_";

/// Built-in field names. Returns `Ok(None)` when the name is not built in, or when the item
/// lacks the section the name belongs to (so a login's custom field "code" still works).
fn builtin_field(view: &CipherView, field: &str) -> Result<Option<Zeroizing<String>>, Error> {
    let login = view.login.as_ref();
    let card = view.card.as_ref();
    let ssh = view.ssh_key.as_ref();
    let value: Option<String> = match field.to_ascii_lowercase().as_str() {
        "name" | "title" => Some(view.name.clone()),
        "notes" => view.notes.clone(),
        "username" => login.and_then(|l| l.username.clone()),
        "password" => match login {
            Some(l) => {
                require_view_password(view)?;
                l.password.clone()
            }
            None => None,
        },
        "uri" | "url" | "website" => login
            .and_then(|l| l.uris.as_ref())
            .and_then(|uris| uris.iter().find_map(|u| u.uri.clone())),
        "number" | "card_number" => card.and_then(|c| c.number.clone()),
        "code" | "cvv" | "security_code" => card.and_then(|c| c.code.clone()),
        "cardholder" | "cardholder_name" => card.and_then(|c| c.cardholder_name.clone()),
        "brand" => card.and_then(|c| c.brand.clone()),
        "expiry" | "expiration" => card.and_then(|c| match (&c.exp_month, &c.exp_year) {
            (Some(m), Some(y)) => Some(format!("{m:0>2}/{y}")),
            _ => None,
        }),
        "private_key" => ssh.map(|k| k.private_key.clone()),
        key if key.starts_with(HISTORY_PREFIX) => {
            let index: Option<usize> = key[HISTORY_PREFIX.len()..].parse().ok();
            let old = index.and_then(|i| view.password_history.as_ref()?.get(i));
            if old.is_some() {
                require_view_password(view)?;
            }
            old.map(|h| h.password.clone())
        }
        "public_key" => ssh.map(|k| k.public_key.clone()),
        "fingerprint" => ssh.map(|k| k.fingerprint.clone()),
        // The other keys of the edit form (identity details, card expiry parts).
        key => {
            let value = crate::edit::value_of(view, key);
            if value.is_some() && crate::SECRET_KEYS.contains(&key) {
                require_view_password(view)?;
            }
            value
        }
    };
    Ok(value.map(Zeroizing::new))
}

/// Custom fields: exact name first, then case-insensitive.
fn custom_field(view: &CipherView, field: &str) -> Result<Option<Zeroizing<String>>, Error> {
    let fields = view.fields.as_deref().unwrap_or_default();
    let found = fields
        .iter()
        .find(|f| f.name.as_deref() == Some(field))
        .or_else(|| {
            fields.iter().find(|f| {
                f.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(field))
            })
        });
    let Some(found) = found else {
        return Ok(None);
    };
    match found.r#type {
        FieldType::Hidden => require_view_password(view)?,
        // Linked fields only point at another field of the item; resolve through it.
        FieldType::Linked => {
            return Err(Error::NotFound(format!(
                "{field:?} is a linked field; ask for the field it links to"
            )));
        }
        FieldType::Text | FieldType::Boolean => {}
    }
    Ok(found.value.clone().map(Zeroizing::new))
}

fn kind_name(kind: &CipherListViewType) -> &'static str {
    match kind {
        CipherListViewType::Login(_) => "login",
        CipherListViewType::SecureNote => "note",
        CipherListViewType::Card(_) => "card",
        CipherListViewType::Identity => "identity",
        CipherListViewType::SshKey => "ssh_key",
        _ => "other",
    }
}
