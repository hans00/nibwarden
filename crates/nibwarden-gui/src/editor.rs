//! The edit form's content (phase2.md P2-M10): the item being created or edited, kept on
//! the UI thread as the user types, and turned into the form's models. The values live
//! here rather than in the models, so adding or removing a row rebuilds the models from
//! what was typed.

use nibwarden_ipc::{
    CustomFieldData, CustomFieldKind, FolderEntry, ITEM_KINDS, ItemData, SECRET_KEYS,
};
use slint::SharedString;
use zeroize::Zeroizing;

use crate::{EditCustom, EditValue, FieldGroup};

/// Keys shown but not saved: they follow from the private key.
const DERIVED: &[&str] = &["public_key", "fingerprint"];

/// The card a value of an item of `kind` is shown in (`Labels.group` in vault.slint),
/// as Bitwarden groups them. Values come in form order, so each group's are together.
pub fn group_of(kind: &str, key: &str) -> &'static str {
    match (kind, key) {
        ("identity", "ssn" | "passport_number" | "license_number") => "identification",
        (
            "identity",
            "email" | "phone" | "address1" | "address2" | "address3" | "city" | "state"
            | "postal_code" | "country",
        ) => "contact",
        ("identity", _) => "personal",
        ("card", _) => "card",
        ("ssh_key", _) => "ssh_key",
        _ => "login",
    }
}

pub struct Edit {
    pub data: ItemData,
    /// Folder ids in the order of `Vault.folders`, `None` first ("No folder").
    folder_ids: Vec<Option<String>>,
    folder_names: Vec<SharedString>,
}

fn keys_of(kind: &str) -> &'static [&'static str] {
    ITEM_KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(&[], |(_, keys)| *keys)
}

impl Edit {
    /// A form for `data` (a new item has no id). Every key of its kind gets a row, except
    /// the derived SSH ones of a key that does not exist yet.
    pub fn new(mut data: ItemData, folders: Vec<FolderEntry>) -> Self {
        let new = data.id.is_none();
        data.values = keys_of(&data.kind)
            .iter()
            .filter(|key| !(new && DERIVED.contains(key)))
            .map(|key| {
                let value = data
                    .values
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                ((*key).to_owned(), value)
            })
            .collect();
        if data.kind == "login" && data.uris.is_empty() {
            data.uris.push(String::new());
        }
        let mut folder_ids = vec![None];
        let mut folder_names = vec![SharedString::new()];
        for folder in folders {
            folder_ids.push(Some(folder.id));
            folder_names.push(folder.name.into());
        }
        Self {
            data,
            folder_ids,
            folder_names,
        }
    }

    pub fn folder_names(&self) -> Vec<SharedString> {
        self.folder_names.clone()
    }

    /// The index of the item's folder in [`Self::folder_names`].
    pub fn folder_index(&self) -> i32 {
        self.folder_ids
            .iter()
            .position(|id| *id == self.data.folder_id)
            .unwrap_or(0) as i32
    }

    pub fn set_folder(&mut self, index: i32) {
        self.data.folder_id = usize::try_from(index)
            .ok()
            .and_then(|i| self.folder_ids.get(i).cloned())
            .flatten();
    }

    pub fn values(&self) -> Vec<EditValue> {
        self.data
            .values
            .iter()
            .map(|(key, value)| EditValue {
                key: key.as_str().into(),
                value: value.as_str().into(),
                secret: SECRET_KEYS.contains(&key.as_str()),
                multiline: matches!(key.as_str(), "private_key" | "public_key"),
                readonly: DERIVED.contains(&key.as_str()),
            })
            .collect()
    }

    /// The cards of [`Self::values`].
    pub fn groups(&self) -> Vec<FieldGroup> {
        let mut groups: Vec<FieldGroup> = Vec::new();
        for (index, (key, _)) in self.data.values.iter().enumerate() {
            let title = group_of(&self.data.kind, key);
            match groups.last_mut() {
                Some(last) if last.title == title => last.count += 1,
                _ => groups.push(FieldGroup {
                    title: title.into(),
                    start: index as i32,
                    count: 1,
                }),
            }
        }
        groups
    }

    pub fn uris(&self) -> Vec<SharedString> {
        self.data.uris.iter().map(|u| u.as_str().into()).collect()
    }

    pub fn fields(&self) -> Vec<EditCustom> {
        self.data
            .fields
            .iter()
            .map(|f| EditCustom {
                name: f.name.as_str().into(),
                value: f.value.as_str().into(),
                kind: match f.kind {
                    CustomFieldKind::Text => "text",
                    CustomFieldKind::Hidden => "hidden",
                    CustomFieldKind::Boolean => "boolean",
                }
                .into(),
            })
            .collect()
    }

    pub fn set_value(&mut self, index: usize, text: &str) {
        if let Some((_, value)) = self.data.values.get_mut(index) {
            *value = Zeroizing::new(text.to_owned());
        }
    }

    pub fn set_uri(&mut self, index: usize, text: &str) {
        if let Some(uri) = self.data.uris.get_mut(index) {
            *uri = text.to_owned();
        }
    }

    pub fn add_uri(&mut self) {
        self.data.uris.push(String::new());
    }

    pub fn remove_uri(&mut self, index: usize) {
        if index < self.data.uris.len() {
            self.data.uris.remove(index);
        }
    }

    pub fn add_field(&mut self, kind: &str) {
        let kind = match kind {
            "hidden" => CustomFieldKind::Hidden,
            "boolean" => CustomFieldKind::Boolean,
            _ => CustomFieldKind::Text,
        };
        let value = if kind == CustomFieldKind::Boolean {
            "false"
        } else {
            ""
        };
        self.data.fields.push(CustomFieldData {
            name: String::new(),
            value: Zeroizing::new(value.to_owned()),
            kind,
        });
    }

    pub fn set_field_name(&mut self, index: usize, text: &str) {
        if let Some(field) = self.data.fields.get_mut(index) {
            field.name = text.to_owned();
        }
    }

    pub fn set_field_value(&mut self, index: usize, text: &str) {
        if let Some(field) = self.data.fields.get_mut(index) {
            field.value = Zeroizing::new(text.to_owned());
        }
    }

    pub fn remove_field(&mut self, index: usize) {
        if index < self.data.fields.len() {
            self.data.fields.remove(index);
        }
    }

    /// What to send to the daemon: without empty websites and the derived SSH values.
    pub fn to_save(&self) -> ItemData {
        let mut data = self.data.clone();
        data.uris.retain(|u| !u.trim().is_empty());
        data.values.retain(|(k, _)| !DERIVED.contains(&k.as_str()));
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> Vec<FolderEntry> {
        vec![FolderEntry {
            id: "f1".into(),
            name: "Work".into(),
        }]
    }

    #[test]
    fn new_items_get_every_key_of_their_kind() {
        let edit = Edit::new(
            ItemData {
                kind: "card".into(),
                ..Default::default()
            },
            folders(),
        );
        let keys: Vec<&str> = edit.data.values.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, keys_of("card"));
        assert!(edit.values().iter().any(|v| v.key == "number" && v.secret));

        let identity = Edit::new(
            ItemData {
                kind: "identity".into(),
                ..Default::default()
            },
            folders(),
        );
        let groups: Vec<(String, i32)> = identity
            .groups()
            .iter()
            .map(|g| (g.title.to_string(), g.count))
            .collect();
        assert_eq!(
            groups,
            [
                ("personal".to_owned(), 6),
                ("identification".to_owned(), 3),
                ("contact".to_owned(), 9)
            ]
        );

        let ssh = Edit::new(
            ItemData {
                kind: "ssh_key".into(),
                ..Default::default()
            },
            folders(),
        );
        assert_eq!(ssh.data.values.len(), 1);
        let login = Edit::new(
            ItemData {
                kind: "login".into(),
                ..Default::default()
            },
            folders(),
        );
        assert_eq!(login.uris(), vec![SharedString::new()]);
    }

    #[test]
    fn folders_and_saving() {
        let mut edit = Edit::new(
            ItemData {
                id: Some("i".into()),
                kind: "ssh_key".into(),
                folder_id: Some("f1".into()),
                values: vec![
                    ("fingerprint".into(), Zeroizing::new("SHA256:x".into())),
                    ("private_key".into(), Zeroizing::new("k".into())),
                ],
                ..Default::default()
            },
            folders(),
        );
        assert_eq!(edit.folder_index(), 1);
        // Form order, derived keys shown read-only on an existing key.
        let rows = edit.values();
        assert_eq!(rows[0].key, "private_key");
        assert!(rows[2].readonly);
        edit.set_folder(0);
        edit.add_field("boolean");
        let saved = edit.to_save();
        assert_eq!(saved.folder_id, None);
        assert_eq!(saved.values.len(), 1);
        assert_eq!(&*saved.fields[0].value, "false");
    }
}
