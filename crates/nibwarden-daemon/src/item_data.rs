//! An item's content between the control protocol and nibwarden-core.

use nibwarden_core::{CustomField, CustomFieldKind, ItemContent};
use nibwarden_ipc::{self as ipc, CustomFieldData, ItemData};

pub fn to_ipc(content: ItemContent) -> ItemData {
    ItemData {
        id: content.id,
        kind: content.kind,
        name: content.name,
        folder_id: content.folder_id,
        favorite: content.favorite,
        reprompt: content.reprompt,
        notes: content.notes,
        values: content.values.into_iter().collect(),
        uris: content.uris,
        fields: content
            .fields
            .into_iter()
            .map(|f| CustomFieldData {
                name: f.name,
                value: f.value,
                kind: match f.kind {
                    CustomFieldKind::Text => ipc::CustomFieldKind::Text,
                    CustomFieldKind::Hidden => ipc::CustomFieldKind::Hidden,
                    CustomFieldKind::Boolean => ipc::CustomFieldKind::Boolean,
                },
            })
            .collect(),
    }
}

pub fn from_ipc(data: ItemData) -> ItemContent {
    ItemContent {
        id: data.id,
        kind: data.kind,
        name: data.name,
        folder_id: data.folder_id,
        favorite: data.favorite,
        reprompt: data.reprompt,
        notes: data.notes,
        values: data.values.into_iter().collect(),
        uris: data.uris,
        fields: data
            .fields
            .into_iter()
            .map(|f| CustomField {
                name: f.name,
                value: f.value,
                kind: match f.kind {
                    ipc::CustomFieldKind::Text => CustomFieldKind::Text,
                    ipc::CustomFieldKind::Hidden => CustomFieldKind::Hidden,
                    ipc::CustomFieldKind::Boolean => CustomFieldKind::Boolean,
                },
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn kind_tables_agree() {
        assert_eq!(nibwarden_ipc::ITEM_KINDS, nibwarden_core::KINDS);
        assert_eq!(nibwarden_ipc::SECRET_KEYS, nibwarden_core::SECRET_KEYS);
    }
}
