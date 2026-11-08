//! Export and import in the main window (phase2.md P2-M13). Shows the `Transfer` global
//! (transfer.slint).
//!
//! Like `sends`, a worker thread talks to the daemon over a connection of its own and
//! runs the file dialogs. An export is written by this process, private to the user from
//! the moment the file exists.

use std::{cell::RefCell, io::Write as _, path::PathBuf, rc::Rc, sync::Arc};

use nibwarden_ipc::{
    Client, ClientError, ErrorCode, ExportFormatData, ExportResult, ImportResult, Request,
    import_needs_password,
};
use slint::{ComponentHandle, Global};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::{Transfer, vault_ui};

/// Mirrors the daemon's limit, to say so before reading a huge file.
const MAX_FILE: u64 = 10 * 1024 * 1024;

enum Command {
    /// Ask where to save, then export.
    Export {
        format: ExportFormatData,
        master_password: Zeroizing<String>,
    },
    ChooseImport,
    Import {
        path: PathBuf,
        password: Option<Zeroizing<String>>,
    },
}

/// Runs `Transfer` code on the UI thread, from any thread.
type Post = Arc<dyn Fn(Box<dyn FnOnce(&Transfer<'_>) + Send>) + Send + Sync>;

thread_local! {
    /// The file chosen for import.
    static CHOSEN: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Connects the window's `Transfer`; `refresh` makes the window show imported items.
pub fn attach<W>(ui: &W, refresh: impl Fn() + Send + Sync + 'static)
where
    W: ComponentHandle + 'static,
    for<'a> Transfer<'a>: Global<'a, W>,
{
    let (tx, rx) = mpsc::unbounded_channel();
    let post: Post = {
        let weak = ui.as_weak();
        Arc::new(move |f| {
            let _ = weak.upgrade_in_event_loop(move |ui| f(&ui.global::<Transfer>()));
        })
    };
    let refresh: Arc<dyn Fn() + Send + Sync> = Arc::new(refresh);
    std::thread::spawn(move || worker(rx, post, refresh));

    let global = ui.global::<Transfer>();
    let send = Rc::new(move |command| {
        let _ = tx.send(command);
    });

    let weak = ui.as_weak();
    global.on_open_export(move || {
        if let Some(ui) = weak.upgrade() {
            let t = ui.global::<Transfer>();
            reset(&t);
            t.set_format(0);
            t.set_export_open(true);
        }
    });

    let weak = ui.as_weak();
    global.on_open_import(move || {
        if let Some(ui) = weak.upgrade() {
            let t = ui.global::<Transfer>();
            reset(&t);
            t.set_import_open(true);
        }
    });

    let weak = ui.as_weak();
    global.on_close(move || {
        if let Some(ui) = weak.upgrade() {
            reset(&ui.global::<Transfer>());
        }
    });

    let weak = ui.as_weak();
    let to = send.clone();
    global.on_export(move || {
        let Some(ui) = weak.upgrade() else { return };
        let t = ui.global::<Transfer>();
        if t.get_busy() {
            return;
        }
        match export_form(&t) {
            Ok(command) => {
                t.set_error("".into());
                t.set_busy(true);
                to(command);
            }
            Err(key) => t.set_error(t.invoke_message(key.into())),
        }
    });

    let to = send.clone();
    global.on_choose_import_file(move || to(Command::ChooseImport));

    let weak = ui.as_weak();
    global.on_import(move || {
        let Some(ui) = weak.upgrade() else { return };
        let t = ui.global::<Transfer>();
        let Some(path) = CHOSEN.with(|c| c.borrow().clone()) else {
            return;
        };
        if t.get_busy() {
            return;
        }
        let password = t
            .get_needs_password()
            .then(|| Zeroizing::new(t.get_import_password().to_string()));
        if password.as_ref().is_some_and(|p| p.is_empty()) {
            t.set_error(t.invoke_message("password-needed".into()));
            return;
        }
        t.set_error("".into());
        t.set_busy(true);
        send(Command::Import { path, password });
    });
}

/// Closed and empty: no password stays in the window.
fn reset(t: &Transfer<'_>) {
    CHOSEN.with(|c| *c.borrow_mut() = None);
    t.set_export_open(false);
    t.set_import_open(false);
    t.set_file_password("".into());
    t.set_file_password_again("".into());
    t.set_master_password("".into());
    t.set_import_name("".into());
    t.set_needs_password(false);
    t.set_import_password("".into());
    t.set_error("".into());
    t.set_busy(false);
    t.set_result("".into());
}

/// The export form as a request, or the key of what is missing (`Transfer.message`).
fn export_form(t: &Transfer<'_>) -> Result<Command, &'static str> {
    let format = match t.get_format() {
        1 => ExportFormatData::Json,
        2 => ExportFormatData::Csv,
        _ => {
            let password = Zeroizing::new(t.get_file_password().to_string());
            if password.is_empty() {
                return Err("file-password");
            }
            if *password != *t.get_file_password_again() {
                return Err("mismatch");
            }
            ExportFormatData::EncryptedJson { password }
        }
    };
    let master_password = Zeroizing::new(t.get_master_password().to_string());
    if master_password.is_empty() {
        return Err("master");
    }
    Ok(Command::Export {
        format,
        master_password,
    })
}

fn worker(
    mut commands: mpsc::UnboundedReceiver<Command>,
    post: Post,
    refresh: Arc<dyn Fn() + Send + Sync>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(async move {
        let mut client: Option<Client> = None;
        while let Some(command) = commands.recv().await {
            if client.is_none() {
                client = vault_ui::connect().await.ok();
            }
            let Some(c) = client.as_mut() else {
                fail(&post, "nibwarden is not running".into());
                continue;
            };
            if let Err(e) = run(c, &post, &*refresh, command).await {
                if !matches!(e, ClientError::Remote(_)) {
                    client = None;
                }
                match known(&e) {
                    Some(key) => fail_key(&post, key),
                    None => fail(&post, e.to_string()),
                }
            }
        }
    });
}

async fn run(
    client: &mut Client,
    post: &Post,
    refresh: &(dyn Fn() + Send + Sync),
    command: Command,
) -> Result<(), ClientError> {
    match command {
        Command::Export {
            format,
            master_password,
        } => {
            let name = match format {
                ExportFormatData::Csv => "nibwarden_export.csv",
                _ => "nibwarden_export.json",
            };
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_file_name(name)
                .save_file()
                .await
            else {
                post(Box::new(|t| t.set_busy(false)));
                return Ok(());
            };
            let path = file.path().to_path_buf();
            let result: ExportResult = client
                .call(Request::VaultExport {
                    format,
                    master_password,
                })
                .await?;
            if let Err(e) = write_private(&path, result.data.as_bytes()) {
                fail(post, format!("cannot write {}: {e}", path.display()));
                return Ok(());
            }
            let items = result.items as i32;
            let shown = path.display().to_string();
            post(Box::new(move |t| {
                t.set_busy(false);
                t.set_master_password("".into());
                t.set_file_password("".into());
                t.set_file_password_again("".into());
                t.set_result(t.invoke_exported(items, shown.into()));
            }));
        }
        Command::ChooseImport => {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("Bitwarden, KeePass", &["json", "csv", "kdbx"])
                .add_filter("*", &["*"])
                .pick_file()
                .await
            else {
                return Ok(());
            };
            let path = file.path().to_path_buf();
            if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_FILE) {
                post(Box::new(|t| {
                    t.set_error(t.invoke_message("too-big".into()))
                }));
                return Ok(());
            }
            let data = match std::fs::read(&path) {
                Ok(data) => Zeroizing::new(data),
                Err(e) => {
                    fail(post, format!("cannot read {}: {e}", path.display()));
                    return Ok(());
                }
            };
            let needs_password = import_needs_password(&data);
            let name = file_name(&path);
            post(Box::new(move |t| {
                CHOSEN.with(|c| *c.borrow_mut() = Some(path));
                t.set_error("".into());
                t.set_import_name(name.into());
                t.set_needs_password(needs_password);
                t.set_import_password("".into());
            }));
        }
        Command::Import { path, password } => {
            let data = match std::fs::read(&path) {
                Ok(data) => Zeroizing::new(data),
                Err(e) => {
                    fail(post, format!("cannot read {}: {e}", path.display()));
                    return Ok(());
                }
            };
            let result: ImportResult = client
                .call(Request::VaultImport {
                    name: file_name(&path),
                    data,
                    password,
                })
                .await?;
            refresh();
            post(Box::new(move |t| {
                t.set_busy(false);
                t.set_import_password("".into());
                t.set_result(t.invoke_imported(result.items as i32, result.folders as i32));
            }));
        }
    }
    Ok(())
}

/// The key (`Transfer.message`) of a daemon error the dialog says in its own words.
fn known(e: &ClientError) -> Option<&'static str> {
    match e {
        ClientError::Remote(body) if body.code == ErrorCode::Denied => {
            match body.message.as_str() {
                "wrong master password" => Some("wrong-master"),
                "wrong file password" | "wrong KeePass password" => Some("wrong-file"),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Shows `message` and ends the busy state.
fn fail(post: &Post, message: String) {
    post(Box::new(move |t| {
        t.set_busy(false);
        t.set_error(message.into());
    }));
}

fn fail_key(post: &Post, key: &'static str) {
    post(Box::new(move |t| {
        t.set_busy(false);
        t.set_error(t.invoke_message(key.into()));
    }));
}

fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let mut file = nibwarden_platform::fs::create_private_file(path)?;
    // A replaced file keeps its old permissions otherwise.
    nibwarden_platform::fs::restrict_file(path)?;
    file.write_all(data)?;
    file.sync_all()
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned())
}
