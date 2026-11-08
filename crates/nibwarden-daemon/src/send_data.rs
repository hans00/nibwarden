//! Sends between the control protocol and nibwarden-core (phase2.md P2-M12).

use nibwarden_core::{NewSend, SendBody, SendInfo, SendSecret};
use nibwarden_ipc::{NewSendData, SendBodyData, SendEntry, SendSecretResult};

pub fn entry(send: SendInfo) -> SendEntry {
    SendEntry {
        id: send.id,
        name: send.name,
        file_name: send.file_name,
        file_size: send.file_size,
        deletion_date: send.deletion_date,
        expiration_date: send.expiration_date,
        access_count: send.access_count,
        max_access_count: send.max_access_count,
        disabled: send.disabled,
        has_password: send.has_password,
        hide_email: send.hide_email,
        notes: send.notes,
    }
}

pub fn secret(secret: SendSecret) -> SendSecretResult {
    SendSecretResult {
        link: secret.link,
        text: secret.text,
        hidden: secret.hidden,
    }
}

pub fn new_send(send: NewSendData) -> NewSend {
    NewSend {
        name: send.name,
        notes: send.notes,
        body: match send.body {
            SendBodyData::Text { text, hidden } => SendBody::Text { text, hidden },
            SendBodyData::File { name, data } => SendBody::File { name, data },
        },
        deletion_days: send.deletion_days,
        max_access_count: send.max_access_count,
        password: send.password,
        hide_email: send.hide_email,
    }
}
