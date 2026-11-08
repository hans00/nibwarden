//! Changing the vault (phase2.md P2-M10): an item's whole content for an edit form,
//! saving it back, favourites, the trash.
//!
//! An item travels as [`ItemContent`]: the type-specific values by key (the same keys
//! [`Account::read_field`] takes), so the form can be built from data. Saving an existing
//! item starts from its stored copy and changes only what the content covers, so passkeys,
//! URI match rules, linked fields, attachments and the password history survive; the SDK
//! moves a changed password into the history.

use std::collections::BTreeMap;

use bitwarden_vault::{
    CardView, CipherCreateRequest, CipherEditRequest, CipherId, CipherPartialEditRequest,
    CipherRepromptType, CipherType, CipherView, CipherViewType, FieldType, FieldView, IdentityView,
    LoginUriView, LoginView, SecureNoteType, SecureNoteView, SshKeyView,
};
use zeroize::Zeroizing;

use crate::{Account, Error, ItemRef, error::SdkContext as _};

/// The item kinds that can be created and edited, with their value keys in form order.
pub const KINDS: &[(&str, &[&str])] = &[
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
    // The public key and fingerprint follow from the private key; they are shown, and
    // ignored when saving.
    ("ssh_key", &["private_key", "public_key", "fingerprint"]),
];

/// Value keys that hold secrets: masked in forms and views, read one at a time.
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

/// Everything about an item that the edit form shows. Empty strings mean "not set".
#[derive(Debug, Clone, Default)]
pub struct ItemContent {
    /// `None` for a new item.
    pub id: Option<String>,
    /// One of [`KINDS`].
    pub kind: String,
    pub name: String,
    pub folder_id: Option<String>,
    pub favorite: bool,
    pub reprompt: bool,
    pub notes: Zeroizing<String>,
    /// Type-specific values by key (see [`KINDS`]); missing keys are empty.
    pub values: BTreeMap<String, Zeroizing<String>>,
    /// Logins only.
    pub uris: Vec<String>,
    /// Custom fields other than linked ones (those are kept as they are).
    pub fields: Vec<CustomField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomFieldKind {
    Text,
    Hidden,
    Boolean,
}

#[derive(Debug, Clone)]
pub struct CustomField {
    pub name: String,
    pub value: Zeroizing<String>,
    pub kind: CustomFieldKind,
}

#[derive(Debug, Clone)]
pub struct FolderInfo {
    pub id: String,
    pub name: String,
}

fn kind_of(r#type: CipherType) -> &'static str {
    match r#type {
        CipherType::Login => "login",
        CipherType::SecureNote => "note",
        CipherType::Card => "card",
        CipherType::Identity => "identity",
        CipherType::SshKey => "ssh_key",
        _ => "other",
    }
}

fn keys_of(kind: &str) -> Option<&'static [&'static str]> {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, keys)| *keys)
}

fn login_slot<'a>(login: &'a mut LoginView, key: &str) -> Option<&'a mut Option<String>> {
    Some(match key {
        "username" => &mut login.username,
        "password" => &mut login.password,
        "totp" => &mut login.totp,
        _ => return None,
    })
}

fn card_slot<'a>(card: &'a mut CardView, key: &str) -> Option<&'a mut Option<String>> {
    Some(match key {
        "cardholder_name" => &mut card.cardholder_name,
        "brand" => &mut card.brand,
        "number" => &mut card.number,
        "exp_month" => &mut card.exp_month,
        "exp_year" => &mut card.exp_year,
        "code" => &mut card.code,
        _ => return None,
    })
}

fn identity_slot<'a>(id: &'a mut IdentityView, key: &str) -> Option<&'a mut Option<String>> {
    Some(match key {
        "title" => &mut id.title,
        "first_name" => &mut id.first_name,
        "middle_name" => &mut id.middle_name,
        "last_name" => &mut id.last_name,
        "username" => &mut id.username,
        "company" => &mut id.company,
        "email" => &mut id.email,
        "phone" => &mut id.phone,
        "ssn" => &mut id.ssn,
        "passport_number" => &mut id.passport_number,
        "license_number" => &mut id.license_number,
        "address1" => &mut id.address1,
        "address2" => &mut id.address2,
        "address3" => &mut id.address3,
        "city" => &mut id.city,
        "state" => &mut id.state,
        "postal_code" => &mut id.postal_code,
        "country" => &mut id.country,
        _ => return None,
    })
}

/// The stored value of one type-specific key, if the item has it.
pub(crate) fn value_of(view: &CipherView, key: &str) -> Option<String> {
    let mut view = view.clone();
    let slot = match kind_of(view.r#type) {
        "login" => view.login.as_mut().and_then(|l| login_slot(l, key)),
        "card" => view.card.as_mut().and_then(|c| card_slot(c, key)),
        "identity" => view.identity.as_mut().and_then(|i| identity_slot(i, key)),
        "ssh_key" => {
            let k = view.ssh_key.as_ref()?;
            return match key {
                "private_key" => Some(k.private_key.clone()),
                "public_key" => Some(k.public_key.clone()),
                "fingerprint" => Some(k.fingerprint.clone()),
                _ => None,
            };
        }
        _ => None,
    };
    slot.and_then(|s| s.clone())
}

/// The card, identity or SSH key values an item has, secret ones as `None`.
pub(crate) fn values_for_view(view: &CipherView) -> Vec<(String, Option<String>)> {
    let kind = kind_of(view.r#type);
    if !matches!(kind, "card" | "identity" | "ssh_key") {
        return Vec::new();
    }
    keys_of(kind)
        .unwrap_or_default()
        .iter()
        .filter_map(|key| {
            let value = value_of(view, key).filter(|v| !v.is_empty())?;
            let shown = (!SECRET_KEYS.contains(key)).then_some(value);
            Some(((*key).to_owned(), shown))
        })
        .collect()
}

fn non_empty(value: Option<&Zeroizing<String>>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(|v| v.to_string())
}

fn content_of(view: &CipherView) -> ItemContent {
    let kind = kind_of(view.r#type);
    let values = keys_of(kind)
        .unwrap_or_default()
        .iter()
        .filter_map(|key| Some(((*key).to_owned(), Zeroizing::new(value_of(view, key)?))))
        .collect();
    ItemContent {
        id: view.id.map(|id| id.to_string()),
        kind: kind.to_owned(),
        name: view.name.clone(),
        folder_id: view.folder_id.map(|id| id.to_string()),
        favorite: view.favorite,
        reprompt: view.reprompt != CipherRepromptType::None,
        notes: Zeroizing::new(view.notes.clone().unwrap_or_default()),
        values,
        uris: view
            .login
            .as_ref()
            .and_then(|l| l.uris.as_ref())
            .map(|uris| uris.iter().filter_map(|u| u.uri.clone()).collect())
            .unwrap_or_default(),
        fields: view
            .fields
            .iter()
            .flatten()
            .filter_map(|f| {
                let kind = match f.r#type {
                    FieldType::Text => CustomFieldKind::Text,
                    FieldType::Hidden => CustomFieldKind::Hidden,
                    FieldType::Boolean => CustomFieldKind::Boolean,
                    FieldType::Linked => return None,
                };
                Some(CustomField {
                    name: f.name.clone().unwrap_or_default(),
                    value: Zeroizing::new(f.value.clone().unwrap_or_default()),
                    kind,
                })
            })
            .collect(),
    }
}

/// The new type-specific data: `old` (the stored item's, when editing the same kind) with
/// the content's values applied.
fn type_data(content: &ItemContent, old: Option<CipherViewType>) -> Result<CipherViewType, Error> {
    let get = |key: &str| non_empty(content.values.get(key));
    let apply = |slot: Option<&mut Option<String>>, key: &str| {
        if let Some(slot) = slot {
            *slot = get(key);
        }
    };
    let keys = keys_of(&content.kind)
        .ok_or_else(|| Error::Invalid(format!("unknown item kind {:?}", content.kind)))?;
    Ok(match content.kind.as_str() {
        "login" => {
            let mut login = match old {
                Some(CipherViewType::Login(login)) => login,
                _ => LoginView {
                    username: None,
                    password: None,
                    password_revision_date: None,
                    uris: None,
                    totp: None,
                    autofill_on_page_load: None,
                    fido2_credentials: None,
                },
            };
            for key in keys {
                apply(login_slot(&mut login, key), key);
            }
            // Keep each remaining URI's match rule; new ones get the default.
            let mut old_uris = login.uris.take().unwrap_or_default();
            let uris: Vec<LoginUriView> = content
                .uris
                .iter()
                .filter(|u| !u.trim().is_empty())
                .map(
                    |uri| match old_uris.iter().position(|o| o.uri.as_ref() == Some(uri)) {
                        Some(i) => old_uris.remove(i),
                        None => LoginUriView {
                            uri: Some(uri.trim().to_owned()),
                            r#match: None,
                            uri_checksum: None,
                        },
                    },
                )
                .collect();
            login.uris = (!uris.is_empty()).then_some(uris);
            CipherViewType::Login(login)
        }
        "note" => CipherViewType::SecureNote(SecureNoteView {
            r#type: SecureNoteType::Generic,
        }),
        "card" => {
            let mut card = match old {
                Some(CipherViewType::Card(card)) => card,
                _ => CardView {
                    cardholder_name: None,
                    exp_month: None,
                    exp_year: None,
                    code: None,
                    brand: None,
                    number: None,
                },
            };
            for key in keys {
                apply(card_slot(&mut card, key), key);
            }
            CipherViewType::Card(card)
        }
        "identity" => {
            let mut identity = match old {
                Some(CipherViewType::Identity(identity)) => identity,
                _ => IdentityView {
                    title: None,
                    first_name: None,
                    middle_name: None,
                    last_name: None,
                    address1: None,
                    address2: None,
                    address3: None,
                    city: None,
                    state: None,
                    postal_code: None,
                    country: None,
                    company: None,
                    email: None,
                    phone: None,
                    ssn: None,
                    username: None,
                    passport_number: None,
                    license_number: None,
                },
            };
            for key in keys {
                apply(identity_slot(&mut identity, key), key);
            }
            CipherViewType::Identity(identity)
        }
        "ssh_key" => CipherViewType::SshKey(ssh_key(get("private_key"), old)?),
        _ => unreachable!("checked by keys_of"),
    })
}

/// The SSH key for a private key: unchanged keeps the stored one, empty on a new item
/// generates an Ed25519 key, anything else is imported (OpenSSH or PKCS#8, unencrypted).
fn ssh_key(private_key: Option<String>, old: Option<CipherViewType>) -> Result<SshKeyView, Error> {
    let private_key = Zeroizing::new(private_key.unwrap_or_default());
    if let Some(CipherViewType::SshKey(old)) = old
        && old.private_key.trim() == private_key.trim()
    {
        return Ok(old);
    }
    let key = if private_key.trim().is_empty() {
        bitwarden_ssh::generator::generate_sshkey(bitwarden_ssh::generator::KeyAlgorithm::Ed25519)
            .map_err(|e| Error::Invalid(format!("cannot generate an SSH key: {e}")))?
    } else {
        bitwarden_ssh::import::import_key(private_key.trim().to_owned(), None)
            .map_err(|e| Error::Invalid(format!("cannot read the SSH private key: {e}")))?
    };
    Ok(SshKeyView {
        private_key: key.private_key,
        public_key: key.public_key,
        fingerprint: key.fingerprint,
    })
}

fn custom_fields(content: &ItemContent, old: Option<Vec<FieldView>>) -> Vec<FieldView> {
    let mut fields: Vec<FieldView> = content
        .fields
        .iter()
        .filter(|f| !f.name.is_empty() || !f.value.is_empty())
        .map(|f| FieldView {
            name: Some(f.name.clone()),
            value: Some(f.value.to_string()),
            r#type: match f.kind {
                CustomFieldKind::Text => FieldType::Text,
                CustomFieldKind::Hidden => FieldType::Hidden,
                CustomFieldKind::Boolean => FieldType::Boolean,
            },
            linked_id: None,
        })
        .collect();
    // Linked fields are not in the form; keep them.
    fields.extend(
        old.into_iter()
            .flatten()
            .filter(|f| f.r#type == FieldType::Linked),
    );
    fields
}

fn parse_id(id: &str) -> Result<CipherId, Error> {
    id.parse()
        .map_err(|_| Error::NotFound(format!("no item with id {id:?}")))
}

fn reprompt_type(reprompt: bool) -> CipherRepromptType {
    if reprompt {
        CipherRepromptType::Password
    } else {
        CipherRepromptType::None
    }
}

impl Account {
    /// One item by id, in the trash or not.
    pub(crate) async fn item_by_id(&self, id: &str) -> Result<CipherView, Error> {
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        self.client
            .vault()
            .ciphers()
            .get(parse_id(id)?)
            .await
            .map_err(|_| Error::NotFound(format!("no item with id {id:?}")))
    }

    /// The item and whether it is in the trash, for approvals before changing it.
    pub async fn item_ref(&self, id: &str) -> Result<(ItemRef, bool), Error> {
        let view = self.item_by_id(id).await?;
        Ok((ItemRef::of(&view), view.deleted_date.is_some()))
    }

    /// Everything about an item, secrets included, for the edit form.
    pub async fn item_content(&self, id: &str) -> Result<(ItemContent, ItemRef), Error> {
        let view = self.item_by_id(id).await?;
        if !view.view_password || !view.edit {
            return Err(Error::Denied(format!(
                "the organisation does not allow you to edit {:?}",
                view.name
            )));
        }
        Ok((content_of(&view), ItemRef::of(&view)))
    }

    /// Creates (no id) or updates an item; returns its id.
    pub async fn save_item(&self, content: &ItemContent) -> Result<String, Error> {
        if content.name.trim().is_empty() {
            return Err(Error::Invalid("an item needs a name".into()));
        }
        let folder_id = match content.folder_id.as_deref().filter(|f| !f.is_empty()) {
            Some(id) => Some(
                id.parse()
                    .map_err(|_| Error::NotFound(format!("no folder with id {id:?}")))?,
            ),
            None => None,
        };
        let notes = non_empty(Some(&content.notes));
        let saved = match &content.id {
            None => {
                if !self.is_unlocked() {
                    return Err(Error::Locked);
                }
                self.client
                    .vault()
                    .ciphers()
                    .create(CipherCreateRequest {
                        organization_id: None,
                        collection_ids: Vec::new(),
                        folder_id,
                        name: content.name.trim().to_owned(),
                        notes,
                        favorite: content.favorite,
                        reprompt: reprompt_type(content.reprompt),
                        r#type: type_data(content, None)?,
                        fields: custom_fields(content, None),
                        archived_date: None,
                    })
                    .await
                    .sdk("create item")?
            }
            Some(id) => {
                let view = self.item_by_id(id).await?;
                if !view.edit || !view.view_password {
                    return Err(Error::Denied(format!(
                        "the organisation does not allow you to edit {:?}",
                        view.name
                    )));
                }
                if view.deleted_date.is_some() {
                    return Err(Error::Invalid(format!(
                        "{:?} is in the trash; restore it first",
                        view.name
                    )));
                }
                if kind_of(view.r#type) != content.kind {
                    return Err(Error::Invalid("an item's kind cannot change".into()));
                }
                let old_fields = view.fields.clone();
                let mut request = CipherEditRequest::try_from(view).sdk("read item")?;
                let old_type = std::mem::replace(
                    &mut request.r#type,
                    CipherViewType::SecureNote(SecureNoteView {
                        r#type: SecureNoteType::Generic,
                    }),
                );
                request.r#type = type_data(content, Some(old_type))?;
                request.name = content.name.trim().to_owned();
                request.notes = notes;
                request.folder_id = folder_id;
                request.favorite = content.favorite;
                request.reprompt = reprompt_type(content.reprompt);
                request.fields = custom_fields(content, old_fields);
                self.client
                    .vault()
                    .ciphers()
                    .edit(request)
                    .await
                    .sdk("save item")?
            }
        };
        Ok(saved.id.map(|id| id.to_string()).unwrap_or_default())
    }

    pub async fn set_favorite(&self, id: &str, favorite: bool) -> Result<(), Error> {
        let view = self.item_by_id(id).await?;
        self.client
            .vault()
            .ciphers()
            .edit_partial(CipherPartialEditRequest {
                id: parse_id(id)?,
                folder_id: view.folder_id,
                favorite,
            })
            .await
            .sdk("change favourite")?;
        Ok(())
    }

    /// Moves an item to the trash, where it can be restored from.
    pub async fn trash_item(&self, id: &str) -> Result<(), Error> {
        self.item_by_id(id).await?;
        self.client
            .vault()
            .ciphers()
            .soft_delete(parse_id(id)?)
            .await
            .sdk("move to trash")
    }

    pub async fn restore_item(&self, id: &str) -> Result<(), Error> {
        self.item_by_id(id).await?;
        self.client
            .vault()
            .ciphers()
            .restore(parse_id(id)?)
            .await
            .sdk("restore item")?;
        Ok(())
    }

    /// Deletes an item for good. Only items already in the trash, so one request cannot
    /// destroy a live item.
    pub async fn delete_item(&self, id: &str) -> Result<(), Error> {
        let view = self.item_by_id(id).await?;
        if view.deleted_date.is_none() {
            return Err(Error::Invalid(format!(
                "{:?} is not in the trash; move it there first",
                view.name
            )));
        }
        self.client
            .vault()
            .ciphers()
            .delete(parse_id(id)?)
            .await
            .sdk("delete item")
    }

    pub async fn folders(&self) -> Result<Vec<FolderInfo>, Error> {
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        let mut folders: Vec<FolderInfo> = self
            .client
            .vault()
            .folders()
            .list()
            .await
            .sdk("list folders")?
            .into_iter()
            .filter_map(|f| {
                Some(FolderInfo {
                    id: f.id?.to_string(),
                    name: f.name,
                })
            })
            .collect();
        folders.sort_by_key(|f| f.name.to_lowercase());
        Ok(folders)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(kind: &str, values: &[(&str, &str)]) -> ItemContent {
        ItemContent {
            kind: kind.into(),
            name: "x".into(),
            values: values
                .iter()
                .map(|(k, v)| ((*k).to_owned(), Zeroizing::new((*v).to_owned())))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn every_kind_key_has_a_slot() {
        for (kind, keys) in KINDS {
            let values: Vec<(&str, &str)> = keys.iter().map(|k| (*k, "v")).collect();
            if *kind == "ssh_key" {
                continue;
            }
            let data = type_data(&content(kind, &values), None).unwrap();
            let mut checked = 0;
            for key in *keys {
                let got = match &data {
                    CipherViewType::Login(l) => {
                        login_slot(&mut l.clone(), key).and_then(|s| s.clone())
                    }
                    CipherViewType::Card(c) => {
                        card_slot(&mut c.clone(), key).and_then(|s| s.clone())
                    }
                    CipherViewType::Identity(i) => {
                        identity_slot(&mut i.clone(), key).and_then(|s| s.clone())
                    }
                    _ => None,
                };
                assert_eq!(got.as_deref(), Some("v"), "{kind}.{key}");
                checked += 1;
            }
            assert_eq!(checked, keys.len());
        }
        for key in SECRET_KEYS {
            assert!(KINDS.iter().any(|(_, keys)| keys.contains(key)), "{key}");
        }
    }

    #[test]
    fn edit_keeps_uri_match_and_clears_emptied_values() {
        let old = CipherViewType::Login(LoginView {
            username: Some("old".into()),
            password: Some("pw".into()),
            password_revision_date: None,
            uris: Some(vec![LoginUriView {
                uri: Some("https://a.test".into()),
                r#match: Some(bitwarden_vault::UriMatchType::Exact),
                uri_checksum: None,
            }]),
            totp: Some("JBSWY3DPEHPK3PXP".into()),
            autofill_on_page_load: None,
            fido2_credentials: None,
        });
        let mut new = content("login", &[("username", "new"), ("password", "pw")]);
        new.uris = vec!["https://b.test".into(), "https://a.test".into(), " ".into()];
        let CipherViewType::Login(login) = type_data(&new, Some(old)).unwrap() else {
            panic!("not a login");
        };
        assert_eq!(login.username.as_deref(), Some("new"));
        assert_eq!(login.totp, None);
        let uris = login.uris.unwrap();
        assert_eq!(uris.len(), 2);
        assert_eq!(uris[0].r#match, None);
        assert_eq!(uris[1].r#match, Some(bitwarden_vault::UriMatchType::Exact));
    }

    #[test]
    fn ssh_key_generated_imported_or_kept() {
        let generated = ssh_key(None, None).unwrap();
        assert!(generated.public_key.starts_with("ssh-ed25519 "));
        let again = ssh_key(
            Some(generated.private_key.clone()),
            Some(CipherViewType::SshKey(generated.clone())),
        )
        .unwrap();
        assert_eq!(again.fingerprint, generated.fingerprint);
        let imported = ssh_key(Some(generated.private_key.clone()), None).unwrap();
        assert_eq!(imported.fingerprint, generated.fingerprint);
        assert!(ssh_key(Some("not a key".into()), None).is_err());
    }
}
