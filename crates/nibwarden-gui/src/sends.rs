//! Sends in the main window (phase2.md P2-M12): list them, show one with its link, create
//! a text or file Send, delete one. Shows the `Sends` global (send.slint).
//!
//! Like `vault_ui`, a worker thread talks to the daemon (over a connection of its own);
//! copying goes through `vault_ui`'s clipboard, which clears it again after a while.

use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

use nibwarden_ipc::{
    Client, ClientError, ErrorCode, NewSendData, Request, SavedResult, SendBodyData, SendEntry,
    SendListResult, SendSecretResult,
};
use slint::{ComponentHandle, Global, Model as _, ModelRc, VecModel};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::{Copied, SendDetail, SendRow, Sends, vault_ui};

/// The deletion choices of the form, in days.
const DAYS: [u32; 6] = [1, 2, 3, 7, 14, 30];
/// Mirrors the daemon's limit, to say so before reading a huge file.
const MAX_FILE: u64 = 10 * 1024 * 1024;

/// Copies a value through the window's clipboard owner.
pub type Copier = Rc<dyn Fn(Zeroizing<String>, Copied)>;

enum Command {
    List,
    /// Read the link and text of the Send with this id.
    Open(String),
    ChooseFile,
    Create {
        send: NewSendData,
        /// The file to read into `send` (a file Send).
        file: Option<PathBuf>,
    },
    Delete(String),
}

/// Runs `Sends` code on the UI thread, from any thread.
type Post = Arc<dyn Fn(Box<dyn FnOnce(&Sends<'_>) + Send>) + Send + Sync>;
type WithSends = Rc<dyn Fn(&mut dyn FnMut(&Sends<'_>))>;

/// UI-thread state.
struct State {
    sends: Vec<SendEntry>,
    rows: Rc<VecModel<SendRow>>,
    /// The id of the Send on the right, and what was read of it.
    open: Option<String>,
    link: Option<Zeroizing<String>>,
    text: Option<Zeroizing<String>>,
    /// The file chosen for a new Send.
    file: Option<PathBuf>,
    copy: Copier,
    sends_global: WithSends,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|state| state.borrow_mut().as_mut().map(f))
}

/// Runs `f` on the window's `Sends` (not while `STATE` is borrowed).
fn sends(mut f: impl FnMut(&Sends<'_>)) {
    if let Some(with) = with_state(|s| s.sends_global.clone()) {
        with(&mut f);
    }
}

/// Connects the window's `Sends`; the worker ends with the window.
pub fn attach<W>(ui: &W, copy: Copier)
where
    W: ComponentHandle + 'static,
    for<'a> Sends<'a>: Global<'a, W>,
{
    let (tx, rx) = mpsc::unbounded_channel();
    let post: Post = {
        let weak = ui.as_weak();
        Arc::new(move |f| {
            let _ = weak.upgrade_in_event_loop(move |ui| f(&ui.global::<Sends>()));
        })
    };
    let sends_global: WithSends = {
        let weak = ui.as_weak();
        Rc::new(move |f| {
            if let Some(ui) = weak.upgrade() {
                f(&ui.global::<Sends>());
            }
        })
    };
    std::thread::spawn(move || worker(rx, post));

    let global = ui.global::<Sends>();
    let rows = Rc::new(VecModel::<SendRow>::default());
    global.set_rows(ModelRc::from(rows.clone()));
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            sends: Vec::new(),
            rows,
            open: None,
            link: None,
            text: None,
            file: None,
            copy,
            sends_global,
        });
    });
    connect(&global, tx);
}

fn connect(ui: &Sends<'_>, tx: mpsc::UnboundedSender<Command>) {
    let send = move |command| {
        let _ = tx.send(command);
    };
    let send = Rc::new(send);

    let to = send.clone();
    ui.on_show(move || {
        sends(|v| {
            v.set_shown(true);
            v.set_message("".into());
            v.set_loading(v.get_rows().row_count() == 0);
        });
        to(Command::List);
    });

    let to = send.clone();
    ui.on_choose(move |index| {
        let entry = with_state(|s| s.sends.get(usize::try_from(index).ok()?).cloned()).flatten();
        let Some(entry) = entry else { return };
        with_state(|s| {
            s.open = Some(entry.id.clone());
            s.link = None;
            s.text = None;
        });
        sends(|v| {
            v.set_selected(index);
            v.set_editing(false);
            v.set_detail(detail(&entry));
            v.set_open(true);
        });
        to(Command::Open(entry.id.clone()));
    });

    ui.on_new_send(|file| {
        with_state(|s| s.file = None);
        sends(|v| {
            v.set_form_file(file);
            v.set_form_name("".into());
            v.set_form_text("".into());
            v.set_form_hidden(false);
            v.set_form_file_name("".into());
            v.set_form_days(3);
            v.set_form_max_accesses("".into());
            v.set_form_password("".into());
            v.set_form_hide_email(false);
            v.set_form_notes("".into());
            v.set_form_error("".into());
            v.set_saving(false);
            v.set_editing(true);
        });
    });

    let to = send.clone();
    ui.on_choose_file(move || to(Command::ChooseFile));

    let to = send.clone();
    ui.on_save(move || {
        let mut command = None;
        sends(|v| {
            if v.get_saving() {
                return;
            }
            match form(v) {
                Ok(c) => {
                    v.set_saving(true);
                    v.set_form_error("".into());
                    command = Some(c);
                }
                Err(problem) => v.set_form_error(problem.into()),
            }
        });
        if let Some(command) = command {
            to(command);
        }
    });

    ui.on_cancel(end_form);

    ui.on_copy_link(|| {
        let copy = with_state(|s| Some((s.copy.clone(), s.link.clone()?))).flatten();
        if let Some((copy, link)) = copy {
            copy(link, Copied::Link);
        }
    });

    ui.on_copy_text(|| {
        let copy = with_state(|s| Some((s.copy.clone(), s.text.clone()?))).flatten();
        if let Some((copy, text)) = copy {
            copy(text, Copied::Other);
        }
    });

    ui.on_open_link(|| {
        if let Some(Some(link)) = with_state(|s| s.link.clone())
            && let Err(e) = nibwarden_platform::browser::open(&link)
        {
            let message = format!("cannot open the link: {e}");
            sends(|v| v.set_message(message.as_str().into()));
        }
    });

    let to = send.clone();
    ui.on_delete(move || {
        if let Some(Some(id)) = with_state(|s| s.open.clone()) {
            to(Command::Delete(id));
        }
    });

    ui.on_forget(forget);
}

/// The form as a request, or what is missing.
fn form(v: &Sends<'_>) -> Result<Command, String> {
    let name = v.get_form_name().trim().to_owned();
    if name.is_empty() {
        return Err("Give the Send a name.".into());
    }
    let max_access_count = match v.get_form_max_accesses().trim() {
        "" => None,
        n => Some(
            n.parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or("The maximum number of accesses must be a whole number.")?,
        ),
    };
    let (body, file) = if v.get_form_file() {
        let file = with_state(|s| s.file.clone())
            .flatten()
            .ok_or("Choose a file to send.")?;
        // Filled in by the worker, which reads the file.
        let body = SendBodyData::File {
            name: String::new(),
            data: Zeroizing::new(Vec::new()),
        };
        (body, Some(file))
    } else {
        let text = Zeroizing::new(v.get_form_text().to_string());
        if text.trim().is_empty() {
            return Err("Write the text to send.".into());
        }
        let hidden = v.get_form_hidden();
        (SendBodyData::Text { text, hidden }, None)
    };
    let password = Zeroizing::new(v.get_form_password().to_string());
    let notes = v.get_form_notes().to_string();
    let send = NewSendData {
        name,
        notes: (!notes.trim().is_empty()).then_some(notes),
        body,
        deletion_days: DAYS[usize::try_from(v.get_form_days())
            .unwrap_or(3)
            .min(DAYS.len() - 1)],
        max_access_count,
        password: (!password.is_empty()).then_some(password),
        hide_email: v.get_form_hide_email(),
    };
    Ok(Command::Create { send, file })
}

fn end_form() {
    with_state(|s| s.file = None);
    sends(|v| {
        v.set_editing(false);
        v.set_saving(false);
        v.set_form_text("".into());
        v.set_form_password("".into());
    });
}

fn close_send() {
    with_state(|s| {
        s.open = None;
        s.link = None;
        s.text = None;
    });
    sends(|v| {
        v.set_open(false);
        v.set_selected(-1);
        v.set_detail(SendDetail::default());
    });
}

/// The vault locked: nothing read stays in the window.
fn forget() {
    end_form();
    close_send();
    with_state(|s| {
        s.sends.clear();
        s.rows.set_vec(Vec::new());
    });
    sends(|v| {
        v.set_shown(false);
        v.set_confirm_delete(false);
        v.set_message("".into());
    });
}

fn detail(entry: &SendEntry) -> SendDetail {
    SendDetail {
        name: entry.name.as_str().into(),
        file: entry.file_name.is_some(),
        file_name: entry.file_name.clone().unwrap_or_default().into(),
        file_size: entry.file_size.clone().unwrap_or_default().into(),
        deletion: vault_ui::local_time(&entry.deletion_date).into(),
        expiration: entry
            .expiration_date
            .as_deref()
            .map(vault_ui::local_time)
            .unwrap_or_default()
            .into(),
        accesses: entry.access_count as i32,
        max_accesses: entry.max_access_count.unwrap_or(0) as i32,
        protected: entry.has_password,
        hide_email: entry.hide_email,
        disabled: entry.disabled,
        notes: entry.notes.clone().unwrap_or_default().into(),
        ..Default::default()
    }
}

/// New Sends from the daemon; `open` (an id) is chosen, or else the one already open
/// stays chosen if it still exists.
fn set_sends(list: Vec<SendEntry>, open: Option<String>) {
    let rows: Vec<SendRow> = list
        .iter()
        .map(|s| SendRow {
            name: s.name.as_str().into(),
            file: s.file_name.is_some(),
            deletion: vault_ui::local_time(&s.deletion_date)
                .split(' ')
                .next()
                .unwrap_or_default()
                .into(),
            disabled: s.disabled,
        })
        .collect();
    let open = open.or_else(|| with_state(|s| s.open.clone()).flatten());
    let index = open.and_then(|id| list.iter().position(|s| s.id == id));
    with_state(|s| {
        s.sends = list;
        s.rows.set_vec(rows);
    });
    sends(|v| v.set_loading(false));
    match index {
        Some(index) => {
            let chosen = with_state(|s| {
                let entry = s.sends[index].clone();
                let same = s.open.as_deref() == Some(entry.id.as_str());
                if !same {
                    s.open = Some(entry.id.clone());
                    s.link = None;
                    s.text = None;
                }
                (entry, same)
            });
            sends(|v| {
                v.set_selected(index as i32);
                v.set_open(true);
                if let Some((entry, same)) = &chosen {
                    let mut shown = detail(entry);
                    if *same {
                        // Keep the link and text already read.
                        let old = v.get_detail();
                        shown.link = old.link;
                        shown.text = old.text;
                        shown.hidden = old.hidden;
                    }
                    v.set_detail(shown);
                }
            });
        }
        None => close_send(),
    }
}

fn worker(mut commands: mpsc::UnboundedReceiver<Command>, post: Post) {
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
                show_problem(&post, "nibwarden is not running".into());
                continue;
            };
            let result = run(c, &post, command).await;
            if let Err(e) = result {
                if matches!(e, ClientError::Remote(ref body) if body.code == ErrorCode::Locked) {
                    // The main window sees the lock on its next refresh.
                    continue;
                }
                if !matches!(e, ClientError::Remote(_)) {
                    client = None;
                }
                let message = e.to_string();
                post(Box::new(move |v| {
                    if v.get_saving() {
                        v.set_saving(false);
                        v.set_form_error(message.into());
                    } else {
                        v.set_message(message.into());
                    }
                }));
            }
        }
    });
}

async fn run(client: &mut Client, post: &Post, command: Command) -> Result<(), ClientError> {
    match command {
        Command::List => list(client, post, None).await?,
        Command::Open(id) => {
            let secret: SendSecretResult = client.call(Request::SendGet { id: id.clone() }).await?;
            post(Box::new(move |v| show_secret(v, &id, secret, false)));
        }
        Command::ChooseFile => {
            let Some(file) = rfd::AsyncFileDialog::new().pick_file().await else {
                return Ok(());
            };
            let path = file.path().to_path_buf();
            let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_FILE);
            let name = file_name(&path);
            post(Box::new(move |v| {
                if too_big {
                    v.set_form_error("The file is larger than 10 MB.".into());
                    return;
                }
                v.set_form_error("".into());
                v.set_form_file_name(name.into());
                with_state(|s| s.file = Some(path));
            }));
        }
        Command::Create { mut send, file } => {
            if let Some(path) = file {
                let data = match std::fs::read(&path) {
                    Ok(data) => Zeroizing::new(data),
                    Err(e) => {
                        let message = format!("cannot read {}: {e}", path.display());
                        post(Box::new(move |v| {
                            v.set_saving(false);
                            v.set_form_error(message.into());
                        }));
                        return Ok(());
                    }
                };
                send.body = SendBodyData::File {
                    name: file_name(&path),
                    data,
                };
            }
            let saved: SavedResult = client.call(Request::SendCreate { send }).await?;
            post(Box::new(|_| end_form()));
            let id = saved.id;
            list(client, post, Some(id.clone())).await?;
            // Like Bitwarden: the new Send's link is ready to paste.
            let secret: SendSecretResult = client.call(Request::SendGet { id: id.clone() }).await?;
            post(Box::new(move |v| show_secret(v, &id, secret, true)));
        }
        Command::Delete(id) => {
            client.call::<()>(Request::SendDelete { id }).await?;
            post(Box::new(|_| close_send()));
            list(client, post, None).await?;
        }
    }
    Ok(())
}

/// Shows the link and text of Send `id`, if it is still the one on the right; `copy`
/// puts the link on the clipboard too.
fn show_secret(v: &Sends<'_>, id: &str, secret: SendSecretResult, copy: bool) {
    let current = with_state(|s| s.open.clone()).flatten();
    if current.as_deref() != Some(id) {
        return;
    }
    let mut detail = v.get_detail();
    detail.link = secret.link.as_str().into();
    detail.text = secret
        .text
        .as_deref()
        .map(String::as_str)
        .unwrap_or_default()
        .into();
    detail.hidden = secret.hidden;
    v.set_detail(detail);
    let copier = with_state(|s| {
        s.link = Some(secret.link.clone());
        s.text = secret.text;
        s.copy.clone()
    });
    if copy && let Some(copier) = copier {
        copier(secret.link, Copied::Link);
    }
}

async fn list(client: &mut Client, post: &Post, open: Option<String>) -> Result<(), ClientError> {
    let result: SendListResult = client.call(Request::SendList).await?;
    post(Box::new(move |_| set_sends(result.sends, open)));
    Ok(())
}

fn show_problem(post: &Post, message: String) {
    post(Box::new(move |v| v.set_message(message.into())));
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned())
}
