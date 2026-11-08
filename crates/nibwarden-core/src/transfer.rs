//! Export and import (phase2.md P2-M13).
//!
//! Exports come from the SDK's exporters (Bitwarden JSON, CSV, password-protected JSON).
//! The SDK cannot read those formats back, so the Bitwarden importer is here: files are
//! parsed into the SDK's `ImportingCipher`, encrypted with its `encrypt_import` and sent to
//! the server's import endpoint in one request. KeePass databases go to the SDK importer.

use std::{collections::HashMap, num::NonZeroU32};

use bitwarden_api_api::models::{
    CipherRequestModel, FolderWithIdRequestModel, ImportCiphersRequestModel, Int32Int32KeyValuePair,
};
use bitwarden_crypto::{
    CompositeEncryptable as _, EncString, IdentifyKey as _, Kdf, KeyDecryptable as _, PinKey,
};
use bitwarden_exporters::{
    Card, CipherType as ImportType, ExportFormat as SdkFormat, Fido2Credential, Field, Identity,
    ImportingCipher, Login, LoginUri, SecureNote, SecureNoteType, SshKey, encrypt_import,
};
use bitwarden_importers::ImportOptions;
use bitwarden_vault::{Cipher, CipherType, Folder, FolderView};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{Account, Error, error::SdkContext as _};

/// Largest file accepted for import, as for a Send file: it travels in one IPC message.
pub const MAX_IMPORT_FILE: usize = 10 * 1024 * 1024;

/// The first bytes of every KeePass 2 (KDBX) database.
const KDBX_MAGIC: [u8; 4] = [0x03, 0xd9, 0xa2, 0x9a];

pub enum ExportFormat {
    Json,
    Csv,
    /// Bitwarden's password-protected JSON: readable only with this password.
    EncryptedJson(Zeroizing<String>),
}

pub struct Export {
    pub data: Zeroizing<String>,
    /// Items in the file (CSV has only logins and secure notes).
    pub items: usize,
}

/// A file read for import, before anything is sent.
pub enum ImportFile {
    Bitwarden(Parsed),
    KeePass {
        data: Zeroizing<Vec<u8>>,
        password: Option<Zeroizing<String>>,
    },
}

/// Items and folders read from a Bitwarden file.
pub struct Parsed {
    ciphers: Vec<ImportingCipher>,
    folders: Vec<String>,
    /// `(cipher index, folder index)`.
    relationships: Vec<(usize, usize)>,
}

impl ImportFile {
    /// Items that will be imported, when known before sending (not for KeePass).
    pub fn items(&self) -> Option<usize> {
        match self {
            ImportFile::Bitwarden(parsed) => Some(parsed.ciphers.len()),
            ImportFile::KeePass { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Imported {
    pub items: usize,
    pub folders: usize,
}

/// Reads a file to import: Bitwarden JSON (also password-protected), Bitwarden CSV or a
/// KeePass database, told apart by their content.
pub fn read_import(
    data: Vec<u8>,
    password: Option<Zeroizing<String>>,
) -> Result<ImportFile, Error> {
    if data.len() > MAX_IMPORT_FILE {
        return Err(Error::Invalid(format!(
            "the file is larger than {} MiB",
            MAX_IMPORT_FILE / 1024 / 1024
        )));
    }
    let data = Zeroizing::new(data);
    if data.starts_with(&KDBX_MAGIC) {
        return Ok(ImportFile::KeePass { data, password });
    }
    let text = std::str::from_utf8(&data).map_err(|_| {
        Error::Invalid("not a Bitwarden JSON or CSV export, or a KeePass database".into())
    })?;
    let text = text.trim_start_matches('\u{feff}');
    let parsed = if text.trim_start().starts_with('{') {
        read_json(text, password.as_deref().map(|p| p.as_str()))?
    } else {
        read_csv(text)?
    };
    if parsed.ciphers.is_empty() {
        return Err(Error::Invalid("the file has no items to import".into()));
    }
    Ok(ImportFile::Bitwarden(parsed))
}

impl Account {
    /// The personal vault (not the trash, not organization items) in `format`.
    pub async fn export_vault(&self, format: ExportFormat) -> Result<Export, Error> {
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        let state = self.client.platform().state();
        let ciphers: Vec<Cipher> = state
            .get::<Cipher>()
            .sdk("open cipher repository")?
            .list()
            .await
            .sdk("read items")?
            .into_iter()
            .filter(|c| c.deleted_date.is_none() && c.organization_id.is_none())
            .collect();
        let folders: Vec<Folder> = state
            .get::<Folder>()
            .sdk("open folder repository")?
            .list()
            .await
            .sdk("read folders")?;
        let items = match format {
            ExportFormat::Csv => ciphers
                .iter()
                .filter(|c| matches!(c.r#type, CipherType::Login | CipherType::SecureNote))
                .count(),
            ExportFormat::Json | ExportFormat::EncryptedJson(_) => ciphers
                .iter()
                .filter(|c| {
                    !matches!(
                        c.r#type,
                        CipherType::BankAccount | CipherType::Passport | CipherType::DriversLicense
                    )
                })
                .count(),
        };
        let format = match format {
            ExportFormat::Json => SdkFormat::Json,
            ExportFormat::Csv => SdkFormat::Csv,
            ExportFormat::EncryptedJson(password) => {
                if password.is_empty() {
                    return Err(Error::Invalid("the file password is empty".into()));
                }
                SdkFormat::EncryptedJson {
                    password: password.as_str().to_owned(),
                }
            }
        };
        let data = self
            .client
            .exporters()
            .export_vault(folders, ciphers, format)
            .await
            .sdk("export")?;
        Ok(Export {
            data: Zeroizing::new(data),
            items,
        })
    }

    /// Adds the items of `file` to the personal vault, then syncs. Folders whose name
    /// already exists are reused.
    pub async fn import(&self, file: ImportFile) -> Result<Imported, Error> {
        if !self.is_unlocked() {
            return Err(Error::Locked);
        }
        let imported = match file {
            ImportFile::Bitwarden(parsed) => self.submit(parsed).await?,
            ImportFile::KeePass { data, password } => {
                let summary = self
                    .client
                    .importers()
                    .import_kdbx(
                        data.to_vec(),
                        password.map(|p| p.as_str().to_owned()),
                        None,
                        ImportOptions {
                            organization_id: None,
                            target_folder: None,
                            target_collection: None,
                            restricted_types: Vec::new(),
                        },
                    )
                    .await
                    .map_err(|e| match e {
                        bitwarden_importers::ImportError::KdbxWrongCredentials => {
                            Error::Denied("wrong KeePass password".into())
                        }
                        bitwarden_importers::ImportError::KdbxInvalidFormat
                        | bitwarden_importers::ImportError::KdbxCorruptOrUnsupported
                        | bitwarden_importers::ImportError::KdbxFileTooLarge => {
                            Error::Invalid(e.to_string())
                        }
                        e => Error::Sdk {
                            context: "import",
                            source: Box::new(e),
                        },
                    })?;
                Imported {
                    items: summary.ciphers.iter().map(|c| c.count as usize).sum(),
                    folders: summary.folders as usize,
                }
            }
        };
        self.sync(true).await?;
        Ok(imported)
    }

    async fn submit(&self, parsed: Parsed) -> Result<Imported, Error> {
        let existing: HashMap<String, _> = self
            .client
            .vault()
            .folders()
            .list()
            .await
            .sdk("list folders")?
            .into_iter()
            .filter_map(|f| Some((f.name, f.id?)))
            .collect();
        let user_id = self
            .client
            .0
            .internal
            .get_user_id()
            .ok_or(Error::LoggedOut)?;
        let new_folders = parsed
            .folders
            .iter()
            .filter(|name| !existing.contains_key(*name))
            .count();
        let items = parsed.ciphers.len();
        let model = {
            let key_store = self.client.0.internal.get_key_store();
            let mut ctx = key_store.context();
            let ciphers = parsed
                .ciphers
                .into_iter()
                .map(|c| {
                    encrypt_import(&mut ctx, c, None, user_id)
                        .map(CipherRequestModel::from)
                        .sdk("encrypt item")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let revision_date = Utc::now();
            let folders = parsed
                .folders
                .into_iter()
                .map(|name| {
                    // An existing folder's id makes the server put the items there.
                    let view = FolderView {
                        id: existing.get(&name).copied(),
                        name,
                        revision_date,
                    };
                    let folder: Folder = view
                        .encrypt_composite(&mut ctx, view.key_identifier())
                        .sdk("encrypt folder")?;
                    Ok(FolderWithIdRequestModel::from(&folder))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            ImportCiphersRequestModel {
                folders: Some(folders),
                ciphers: Some(ciphers),
                folder_relationships: Some(
                    parsed
                        .relationships
                        .iter()
                        .map(|&(cipher, folder)| Int32Int32KeyValuePair {
                            key: Some(cipher as i32),
                            value: Some(folder as i32),
                        })
                        .collect(),
                ),
            }
        };
        self.client
            .0
            .internal
            .get_api_configurations()
            .api_client
            .import_ciphers_api()
            .post_import(Some(model))
            .await
            .sdk("import")?;
        Ok(Imported {
            items,
            folders: new_folders,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EncryptedHeader {
    #[serde(default)]
    encrypted: bool,
    #[serde(default)]
    password_protected: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EncryptedJson {
    salt: String,
    kdf_type: u32,
    kdf_iterations: u32,
    kdf_memory: Option<u32>,
    kdf_parallelism: Option<u32>,
    #[serde(rename = "encKeyValidation_DO_NOT_EDIT")]
    enc_key_validation: EncString,
    data: EncString,
}

fn read_json(text: &str, password: Option<&str>) -> Result<Parsed, Error> {
    let header: EncryptedHeader = serde_json::from_str(text).map_err(not_bitwarden)?;
    if !header.encrypted {
        return parse_json(text);
    }
    if !header.password_protected {
        return Err(Error::Invalid(
            "this export is encrypted with its account's key; export it again as \
             password protected (or as plain JSON) to import it"
                .into(),
        ));
    }
    let password = password.ok_or_else(|| Error::Denied("this file needs its password".into()))?;
    let file: EncryptedJson = serde_json::from_str(text).map_err(not_bitwarden)?;
    let nonzero = |n: Option<u32>| {
        n.and_then(NonZeroU32::new)
            .ok_or_else(|| Error::Invalid("the file's key settings are invalid".into()))
    };
    let kdf = match file.kdf_type {
        0 => Kdf::PBKDF2 {
            iterations: nonzero(Some(file.kdf_iterations))?,
        },
        1 => Kdf::Argon2id {
            iterations: nonzero(Some(file.kdf_iterations))?,
            memory: nonzero(file.kdf_memory)?,
            parallelism: nonzero(file.kdf_parallelism)?,
        },
        _ => return Err(Error::Invalid("the file's key settings are invalid".into())),
    };
    // Bitwarden uses the base64 text of the salt, not its bytes.
    let key = PinKey::derive(password.as_bytes(), file.salt.as_bytes(), &kdf)
        .sdk("derive the file key")?;
    let check: Result<String, _> = file.enc_key_validation.decrypt_with_key(&key);
    if check.is_err() {
        return Err(Error::Denied("wrong file password".into()));
    }
    let inner: String = file.data.decrypt_with_key(&key).sdk("decrypt the file")?;
    parse_json(&Zeroizing::new(inner))
}

fn not_bitwarden(e: serde_json::Error) -> Error {
    Error::Invalid(format!("not a Bitwarden JSON export: {e}"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonExport {
    #[serde(default)]
    folders: Vec<JsonFolder>,
    #[serde(default)]
    items: Vec<JsonItem>,
}

#[derive(Deserialize)]
struct JsonFolder {
    id: Option<String>,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonItem {
    folder_id: Option<String>,
    #[serde(default)]
    name: String,
    notes: Option<String>,
    r#type: u8,
    login: Option<JsonLogin>,
    card: Option<JsonCard>,
    identity: Option<JsonIdentity>,
    ssh_key: Option<JsonSshKey>,
    #[serde(default)]
    favorite: bool,
    #[serde(default)]
    reprompt: u8,
    #[serde(default)]
    fields: Option<Vec<JsonField>>,
    revision_date: Option<DateTime<Utc>>,
    creation_date: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonLogin {
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    uris: Option<Vec<JsonUri>>,
    totp: Option<String>,
    #[serde(default)]
    fido2_credentials: Option<Vec<JsonPasskey>>,
}

#[derive(Deserialize)]
struct JsonUri {
    uri: Option<String>,
    r#match: Option<u8>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonPasskey {
    credential_id: String,
    key_type: String,
    key_algorithm: String,
    key_curve: String,
    key_value: String,
    rp_id: String,
    user_handle: Option<String>,
    user_name: Option<String>,
    #[serde(default)]
    counter: serde_json::Value,
    rp_name: Option<String>,
    user_display_name: Option<String>,
    #[serde(default)]
    discoverable: serde_json::Value,
    creation_date: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonCard {
    cardholder_name: Option<String>,
    exp_month: Option<String>,
    exp_year: Option<String>,
    code: Option<String>,
    brand: Option<String>,
    number: Option<String>,
}

/// Bitwarden's identity fields, which match the SDK's names.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonIdentity {
    title: Option<String>,
    first_name: Option<String>,
    middle_name: Option<String>,
    last_name: Option<String>,
    address1: Option<String>,
    address2: Option<String>,
    address3: Option<String>,
    city: Option<String>,
    state: Option<String>,
    postal_code: Option<String>,
    country: Option<String>,
    company: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    ssn: Option<String>,
    username: Option<String>,
    passport_number: Option<String>,
    license_number: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonSshKey {
    private_key: String,
    public_key: String,
    key_fingerprint: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonField {
    name: Option<String>,
    value: Option<String>,
    #[serde(default)]
    r#type: u8,
    linked_id: Option<u32>,
}

/// A JSON value written as a string or a number (passkey counters and flags vary).
fn loose_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn parse_json(text: &str) -> Result<Parsed, Error> {
    let export: JsonExport = serde_json::from_str(text).map_err(not_bitwarden)?;
    let folder_index: HashMap<String, usize> = export
        .folders
        .iter()
        .enumerate()
        .filter_map(|(i, f)| Some((f.id.clone()?, i)))
        .collect();
    let mut parsed = Parsed {
        ciphers: Vec::new(),
        folders: export.folders.into_iter().map(|f| f.name).collect(),
        relationships: Vec::new(),
    };
    let now = Utc::now();
    for item in export.items {
        let r#type = match item.r#type {
            1 => {
                let login = item.login.unwrap_or(JsonLogin {
                    username: None,
                    password: None,
                    uris: None,
                    totp: None,
                    fido2_credentials: None,
                });
                ImportType::Login(Box::new(Login {
                    username: login.username,
                    password: login.password,
                    login_uris: login
                        .uris
                        .unwrap_or_default()
                        .into_iter()
                        .map(|u| LoginUri {
                            uri: u.uri,
                            r#match: u.r#match,
                        })
                        .collect(),
                    totp: login.totp,
                    fido2_credentials: login.fido2_credentials.map(|keys| {
                        keys.into_iter()
                            .map(|k| Fido2Credential {
                                credential_id: k.credential_id,
                                key_type: k.key_type,
                                key_algorithm: k.key_algorithm,
                                key_curve: k.key_curve,
                                key_value: k.key_value,
                                rp_id: k.rp_id,
                                user_handle: k.user_handle,
                                user_name: k.user_name,
                                counter: loose_string(&k.counter).parse().unwrap_or(0),
                                rp_name: k.rp_name,
                                user_display_name: k.user_display_name,
                                discoverable: loose_string(&k.discoverable),
                                creation_date: k.creation_date.unwrap_or(now),
                            })
                            .collect()
                    }),
                }))
            }
            2 => ImportType::SecureNote(Box::new(SecureNote {
                r#type: SecureNoteType::Generic,
            })),
            3 => {
                let Some(c) = item.card else { continue };
                ImportType::Card(Box::new(Card {
                    cardholder_name: c.cardholder_name,
                    exp_month: c.exp_month,
                    exp_year: c.exp_year,
                    code: c.code,
                    brand: c.brand,
                    number: c.number,
                }))
            }
            4 => {
                let Some(i) = item.identity else { continue };
                ImportType::Identity(Box::new(Identity {
                    title: i.title,
                    first_name: i.first_name,
                    middle_name: i.middle_name,
                    last_name: i.last_name,
                    address1: i.address1,
                    address2: i.address2,
                    address3: i.address3,
                    city: i.city,
                    state: i.state,
                    postal_code: i.postal_code,
                    country: i.country,
                    company: i.company,
                    email: i.email,
                    phone: i.phone,
                    ssn: i.ssn,
                    username: i.username,
                    passport_number: i.passport_number,
                    license_number: i.license_number,
                }))
            }
            5 => {
                let Some(k) = item.ssh_key else { continue };
                ImportType::SshKey(Box::new(SshKey {
                    private_key: k.private_key,
                    public_key: k.public_key,
                    fingerprint: k.key_fingerprint,
                }))
            }
            // Types this version does not know.
            _ => continue,
        };
        let index = parsed.ciphers.len();
        if let Some(folder) = item.folder_id.and_then(|id| folder_index.get(&id).copied()) {
            parsed.relationships.push((index, folder));
        }
        parsed.ciphers.push(ImportingCipher {
            folder_id: None,
            name: item.name,
            notes: item.notes,
            r#type,
            favorite: item.favorite,
            reprompt: item.reprompt,
            fields: item
                .fields
                .unwrap_or_default()
                .into_iter()
                .map(|f| Field {
                    name: f.name,
                    value: f.value,
                    r#type: f.r#type,
                    linked_id: f.linked_id,
                })
                .collect(),
            revision_date: item.revision_date.unwrap_or(now),
            creation_date: item.creation_date.unwrap_or(now),
            deleted_date: None,
        });
    }
    Ok(parsed)
}

/// Bitwarden's CSV columns.
#[derive(Deserialize)]
struct CsvRow {
    #[serde(default)]
    folder: String,
    #[serde(default)]
    favorite: String,
    #[serde(default)]
    r#type: String,
    name: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    fields: String,
    #[serde(default)]
    reprompt: String,
    #[serde(default)]
    login_uri: String,
    #[serde(default)]
    login_username: String,
    #[serde(default)]
    login_password: String,
    #[serde(default)]
    login_totp: String,
}

fn read_csv(text: &str) -> Result<Parsed, Error> {
    let not_csv = |e: csv::Error| Error::Invalid(format!("not a Bitwarden CSV export: {e}"));
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers = reader.headers().map_err(not_csv)?;
    if !headers.iter().any(|h| h == "name") || !headers.iter().any(|h| h == "login_password") {
        return Err(Error::Invalid(
            "not a Bitwarden CSV export (it needs name and login_password columns)".into(),
        ));
    }
    let mut parsed = Parsed {
        ciphers: Vec::new(),
        folders: Vec::new(),
        relationships: Vec::new(),
    };
    let now = Utc::now();
    let some = |s: String| (!s.is_empty()).then_some(s);
    for row in reader.deserialize::<CsvRow>() {
        let row = row.map_err(not_csv)?;
        let row = &row;
        let r#type = if row.r#type == "note" {
            ImportType::SecureNote(Box::new(SecureNote {
                r#type: SecureNoteType::Generic,
            }))
        } else {
            ImportType::Login(Box::new(Login {
                username: some(row.login_username.clone()),
                password: some(row.login_password.clone()),
                login_uris: row
                    .login_uri
                    .split(',')
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .map(|u| LoginUri {
                        uri: Some(u.to_owned()),
                        r#match: None,
                    })
                    .collect(),
                totp: some(row.login_totp.clone()),
                fido2_credentials: None,
            }))
        };
        let index = parsed.ciphers.len();
        if !row.folder.is_empty() {
            let folder = match parsed.folders.iter().position(|f| *f == row.folder) {
                Some(i) => i,
                None => {
                    parsed.folders.push(row.folder.clone());
                    parsed.folders.len() - 1
                }
            };
            parsed.relationships.push((index, folder));
        }
        parsed.ciphers.push(ImportingCipher {
            folder_id: None,
            name: if row.name.is_empty() {
                "--".into()
            } else {
                row.name.clone()
            },
            notes: some(row.notes.clone()),
            r#type,
            favorite: row.favorite == "1",
            reprompt: if row.reprompt == "1" { 1 } else { 0 },
            fields: csv_fields(&row.fields),
            revision_date: now,
            creation_date: now,
            deleted_date: None,
        });
    }
    Ok(parsed)
}

/// Bitwarden's CSV `fields` column: one `name: value` per line, all text fields.
fn csv_fields(column: &str) -> Vec<Field> {
    column
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let (name, value) = line.split_once(": ").unwrap_or((line, ""));
            Field {
                name: Some(name.to_owned()),
                value: Some(value.to_owned()),
                r#type: 0,
                linked_id: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(file: ImportFile) -> Parsed {
        match file {
            ImportFile::Bitwarden(p) => p,
            ImportFile::KeePass { .. } => panic!("not a Bitwarden file"),
        }
    }

    #[test]
    fn reads_bitwarden_json() {
        let json = r#"{
          "encrypted": false,
          "folders": [{"id": "f1", "name": "Work"}],
          "items": [
            {"id": "a", "folderId": "f1", "type": 1, "name": "GitHub", "notes": null,
             "favorite": true, "reprompt": 0,
             "login": {"username": "alice", "password": "pw", "totp": null,
                       "uris": [{"uri": "https://github.com", "match": null}],
                       "fido2Credentials": []},
             "fields": [{"name": "pin", "value": "1234", "type": 1, "linkedId": null}],
             "revisionDate": "2024-01-30T17:55:36.150Z", "creationDate": "2024-01-30T17:55:36.150Z",
             "deletedDate": null},
            {"id": "b", "folderId": null, "type": 2, "name": "Note", "notes": "text",
             "secureNote": {"type": 0}, "favorite": false, "reprompt": 1},
            {"id": "c", "type": 9, "name": "Unknown kind"}
          ]
        }"#;
        let file = read_import(json.as_bytes().to_vec(), None).unwrap();
        assert_eq!(file.items(), Some(2));
        let p = parsed(file);
        assert_eq!(p.folders, ["Work"]);
        assert_eq!(p.relationships, [(0, 0)]);
        let ImportType::Login(login) = &p.ciphers[0].r#type else {
            panic!("not a login")
        };
        assert_eq!(login.password.as_deref(), Some("pw"));
        assert_eq!(
            login.login_uris[0].uri.as_deref(),
            Some("https://github.com")
        );
        assert!(p.ciphers[0].favorite);
        assert_eq!(p.ciphers[0].fields[0].value.as_deref(), Some("1234"));
        assert_eq!(p.ciphers[1].reprompt, 1);
        assert_eq!(p.ciphers[1].notes.as_deref(), Some("text"));
    }

    #[test]
    fn reads_bitwarden_csv() {
        let csv = "folder,favorite,type,name,notes,fields,reprompt,login_uri,login_username,login_password,login_totp\n\
                   Work,1,login,GitHub,,\"pin: 1234\nmode: dark\",0,\"https://github.com,https://gh.io\",alice,pw,\n\
                   ,,note,Note,\"line 1\nline 2\",,1,,,,\n\
                   Work,,login,GitLab,,,,,bob,pw2,JBSWY3DPEHPK3PXP\n";
        let p = parsed(read_import(csv.as_bytes().to_vec(), None).unwrap());
        assert_eq!(p.ciphers.len(), 3);
        assert_eq!(p.folders, ["Work"]);
        assert_eq!(p.relationships, [(0, 0), (2, 0)]);
        let ImportType::Login(login) = &p.ciphers[0].r#type else {
            panic!("not a login")
        };
        assert_eq!(login.login_uris.len(), 2);
        assert_eq!(p.ciphers[0].fields.len(), 2);
        assert_eq!(p.ciphers[0].fields[1].name.as_deref(), Some("mode"));
        assert!(matches!(p.ciphers[1].r#type, ImportType::SecureNote(_)));
        assert_eq!(p.ciphers[1].notes.as_deref(), Some("line 1\nline 2"));
        assert_eq!(p.ciphers[1].reprompt, 1);
    }

    #[test]
    fn rejects_other_files() {
        assert!(matches!(
            read_import(b"a,b\n1,2\n".to_vec(), None),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            read_import(
                b"{\"encrypted\": true, \"encKeyValidation_DO_NOT_EDIT\": \"x\"}".to_vec(),
                None
            ),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            read_import(vec![0xff, 0xfe, 0x00], None),
            Err(Error::Invalid(_))
        ));
        let keepass = [&KDBX_MAGIC[..], &[0; 8]].concat();
        assert!(matches!(
            read_import(keepass, None),
            Ok(ImportFile::KeePass { .. })
        ));
    }
}
