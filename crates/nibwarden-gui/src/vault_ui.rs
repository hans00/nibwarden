//! The vault in a window, shared by quick access (`quick.rs`) and the main window
//! (`vault_window.rs`): search, categories, open an item, copy a field, and clear the
//! clipboard again after a while. Both windows show the `Vault` global (vault.slint).
//!
//! The main thread runs the window; a worker thread talks to the daemon over the control
//! socket (as an ordinary client) and owns the clipboard. Closing the window does not
//! end the process while a copied secret is still on the clipboard: on Linux the program
//! that set the clipboard has to keep serving it, and clears it when the time is up.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use nibwarden_ipc::{
    Client, ClientError, ErrorCode, FolderListResult, ItemData, ItemInfo, ItemListResult, ItemView,
    LockState, PassphraseParams, PasswordParams, Request, SavedResult, StatusResult, SyncResult,
    TotpResult, ValueResult,
};
use nibwarden_platform::clipboard::Clipboard;
use slint::{ComponentHandle, Global, Model as _, ModelRc, VecModel};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::{
    Category, Copied, CopyField, EditCustom, EditProblem, EditValue, FieldGroup, FieldKind,
    FieldLabel, ItemField, QuickItem, QuickNotice, QuickState, UnlockError, Vault,
    editor::{Edit, group_of},
    scan::{self, Problem},
    search,
};

/// How long "Password copied" stays.
const TOAST: Duration = Duration::from_millis(1600);
/// With clearing turned off, how often to check whether the clipboard still holds our
/// secret (the process can end once something else replaced it).
const STILL_OURS: Duration = Duration::from_secs(2);

/// A field of an item, as the worker reads it from the daemon.
#[derive(Debug, Clone)]
enum FieldKey {
    Username,
    Password,
    Code,
    Notes,
    /// A custom field by name.
    Custom(String),
    /// A card, identity or SSH key value by key.
    Value(String),
}

enum Command {
    Load,
    Unlock(Zeroizing<String>),
    Open(String),
    /// Copy a field read from the daemon.
    Copy {
        id: String,
        key: FieldKey,
    },
    /// Copy a value the window already shows (a website, a text field).
    CopyText(String),
    /// Copy a value from elsewhere in the window (a Send's link), saying what it is.
    CopyAs(Zeroizing<String>, Copied),
    /// Show a secret field of the open item.
    Reveal {
        id: String,
        index: usize,
        key: FieldKey,
    },
    /// Fetch the current one-time code of the open item.
    Code {
        id: String,
        index: usize,
    },
    Lock,
    SignOut,
    Sync,
    /// Open the edit form for an item.
    Edit(String),
    /// Open the form for a new item of a kind, in the folder of this name.
    New {
        kind: String,
        folder: Option<String>,
        favorite: bool,
    },
    Save(ItemData),
    /// Generate a password or passphrase for the generator.
    Generate(Request),
    /// Fill row `index` of the form from outside the window.
    Fill {
        index: usize,
        source: Source,
    },
    Favorite {
        id: String,
        favorite: bool,
    },
    Trash(String),
    Restore(String),
    Delete(String),
    /// Catch up with the daemon: the vault may have locked, unlocked or synced meanwhile.
    Refresh,
    /// The window is gone; keep serving or clearing the clipboard, then stop.
    Quit,
}

/// Where a value for the form comes from.
enum Source {
    /// A key file the user picks, or one dropped on the window.
    ChooseKey,
    KeyFile(PathBuf),
    /// An authenticator QR code: on the screen, in an image the user picks, in one dropped.
    Screen,
    ChooseImage,
    ImageFile(PathBuf),
}

/// Runs `Vault` code on the UI thread, from any thread.
type Post = Arc<dyn Fn(Box<dyn FnOnce(&Vault<'_>) + Send>) + Send + Sync>;
/// Runs `Vault` code right away, on the UI thread.
type WithVault = Rc<dyn Fn(&mut dyn FnMut(&Vault<'_>))>;

/// The vault controller of one window; dropping it does not stop the worker, `finish`
/// does.
pub struct Attached {
    commands: mpsc::UnboundedSender<Command>,
    worker: std::thread::JoinHandle<()>,
    _ticker: slint::Timer,
}

impl Attached {
    /// Copies through this window's clipboard owner, which clears it again after a while.
    pub fn copier(&self) -> crate::sends::Copier {
        let tx = self.commands.clone();
        Rc::new(move |value, what| {
            let _ = tx.send(Command::CopyAs(value, what));
        })
    }

    /// A function that asks the controller to catch up with the daemon (`Refresh`).
    pub fn refresher(&self) -> impl Fn() + 'static {
        let tx = self.commands.clone();
        move || {
            let _ = tx.send(Command::Refresh);
        }
    }

    /// A function for files dropped on the window: while an SSH key is edited, a key
    /// file; while a login is, an image with its QR code.
    pub fn dropper(&self) -> impl Fn(PathBuf) + 'static {
        let tx = self.commands.clone();
        move |path| {
            let target = with_state(|s| {
                let edit = s.edit.as_ref()?;
                let (key, source): (&str, fn(PathBuf) -> Source) = match edit.data.kind.as_str() {
                    "ssh_key" => ("private_key", Source::KeyFile),
                    "login" => ("totp", Source::ImageFile),
                    _ => return None,
                };
                let index = edit.data.values.iter().position(|(k, _)| k == key)?;
                Some((index, source))
            })
            .flatten();
            if let Some((index, source)) = target {
                let _ = tx.send(Command::Fill {
                    index,
                    source: source(path),
                });
            }
        }
    }

    /// After the window closed: let the worker finish with the clipboard, then end.
    pub fn finish(self) {
        drop(self._ticker);
        STATE.with(|state| state.borrow_mut().take());
        let _ = self.commands.send(Command::Quit);
        let _ = self.worker.join();
    }
}

/// Connects `ui`'s `Vault` to the daemon and loads it. `close` is left to the window.
pub fn attach<W>(ui: &W, settings: &nibwarden_config::Config) -> Attached
where
    W: ComponentHandle + 'static,
    for<'a> Vault<'a>: Global<'a, W>,
{
    let (commands, command_rx) = mpsc::unbounded_channel();
    let post: Post = {
        let weak = ui.as_weak();
        Arc::new(move |f| {
            let _ = weak.upgrade_in_event_loop(move |ui| f(&ui.global::<Vault>()));
        })
    };
    let with_vault: WithVault = {
        let weak = ui.as_weak();
        Rc::new(move |f| {
            if let Some(ui) = weak.upgrade() {
                f(&ui.global::<Vault>());
            }
        })
    };
    let clear_after = settings.clipboard.clear_after();
    let worker = {
        let post = post.clone();
        std::thread::spawn(move || worker(command_rx, post, clear_after))
    };

    let vault = ui.global::<Vault>();
    vault.set_can_scan_screen(scan::CAN_SCAN_SCREEN);
    let items = Rc::new(VecModel::<QuickItem>::default());
    let fields = Rc::new(VecModel::<ItemField>::default());
    let groups = Rc::new(VecModel::<FieldGroup>::default());
    let categories = Rc::new(VecModel::<Category>::default());
    let form = Form::default();
    vault.set_items(ModelRc::from(items.clone()));
    vault.set_fields(ModelRc::from(fields.clone()));
    vault.set_groups(ModelRc::from(groups.clone()));
    vault.set_categories(ModelRc::from(categories.clone()));
    vault.set_edit_values(ModelRc::from(form.values.clone()));
    vault.set_edit_groups(ModelRc::from(form.groups.clone()));
    vault.set_edit_uris(ModelRc::from(form.uris.clone()));
    vault.set_edit_fields(ModelRc::from(form.fields.clone()));
    vault.set_folders(ModelRc::from(form.folders.clone()));
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            all: Vec::new(),
            shown: Vec::new(),
            items,
            open: None,
            fields,
            groups,
            categories,
            category_keys: Vec::new(),
            category: CategoryKey::All,
            query: String::new(),
            edit: None,
            form,
            delete_target: None,
            vault: with_vault,
        });
    });
    connect_callbacks(&vault, &commands);
    // One-time codes count down once a second while an item is open.
    let ticker = slint::Timer::default();
    {
        let tx = commands.clone();
        ticker.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                tick_codes(&tx);
            },
        );
    }
    let _ = commands.send(Command::Load);
    Attached {
        commands,
        worker,
        _ticker: ticker,
    }
}

/// UI-thread state: everything the daemon listed, which of it the search shows, and the
/// open item. The worker hands data over with `upgrade_in_event_loop`.
struct State {
    all: Vec<ItemInfo>,
    shown: Vec<usize>,
    items: Rc<VecModel<QuickItem>>,
    open: Option<OpenItem>,
    fields: Rc<VecModel<ItemField>>,
    groups: Rc<VecModel<FieldGroup>>,
    categories: Rc<VecModel<Category>>,
    category_keys: Vec<CategoryKey>,
    category: CategoryKey,
    query: String,
    /// The item in the edit form, as typed so far.
    edit: Option<Edit>,
    form: Form,
    /// The item the "delete permanently?" question is about.
    delete_target: Option<String>,
    vault: WithVault,
}

/// The edit form's models.
#[derive(Default)]
struct Form {
    values: Rc<VecModel<EditValue>>,
    groups: Rc<VecModel<FieldGroup>>,
    uris: Rc<VecModel<slint::SharedString>>,
    fields: Rc<VecModel<EditCustom>>,
    folders: Rc<VecModel<slint::SharedString>>,
}

/// A sidebar entry of the main window; quick access always shows `All`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CategoryKey {
    All,
    Favorites,
    Kind(String),
    Folder(String),
    Trash,
}

impl CategoryKey {
    fn matches(&self, item: &ItemInfo) -> bool {
        match self {
            Self::Trash => item.trashed,
            _ if item.trashed => false,
            Self::All => true,
            Self::Favorites => item.favorite,
            Self::Kind(kind) => item.kind == *kind,
            Self::Folder(folder) => item.folder.as_deref() == Some(folder.as_str()),
        }
    }
}

/// The order kinds are listed in, as 1Password and Bitwarden do.
const KINDS: [&str; 5] = ["login", "card", "identity", "note", "ssh_key"];

struct OpenItem {
    view: ItemView,
    /// How to read each row of `fields`; `None` for values already shown (websites,
    /// text fields), which are copied as shown.
    keys: Vec<Option<FieldKey>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// Counts toasts, so an older one's timer does not hide a newer one.
    static TOAST_GENERATION: Cell<u64> = const { Cell::new(0) };
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|state| state.borrow_mut().as_mut().map(f))
}

/// Runs `f` on the window's `Vault`, on the UI thread (not while `STATE` is borrowed).
fn vault(mut f: impl FnMut(&Vault<'_>)) {
    if let Some(with_vault) = with_state(|s| s.vault.clone()) {
        with_vault(&mut f);
    }
}

fn connect_callbacks(ui: &Vault<'_>, commands: &mpsc::UnboundedSender<Command>) {
    ui.on_search(move |query| {
        with_state(|s| s.query = query.to_string());
        refilter();
        vault(|v| v.set_selected(0));
        follow_open();
    });

    ui.on_choose_category(move |index| {
        let chosen = with_state(|s| {
            let key = s.category_keys.get(usize::try_from(index).ok()?)?.clone();
            s.category = key;
            Some(())
        });
        if chosen.flatten().is_some() {
            refilter();
            vault(|v| {
                v.set_category(index);
                v.set_selected(0);
            });
            follow_open();
        }
    });

    let tx = commands.clone();
    ui.on_lock(move || {
        let _ = tx.send(Command::Lock);
    });

    let tx = commands.clone();
    ui.on_sign_out(move || {
        let _ = tx.send(Command::SignOut);
    });

    let tx = commands.clone();
    ui.on_sync(move || {
        vault(|v| v.set_syncing(true));
        let _ = tx.send(Command::Sync);
    });

    let tx = commands.clone();
    ui.on_open(move |row| {
        if let Some(id) = row_id(row) {
            end_edit();
            let _ = tx.send(Command::Open(id));
        }
    });

    let tx = commands.clone();
    ui.on_copy(move |row, field| {
        let key = match field {
            CopyField::Username => FieldKey::Username,
            CopyField::Password => FieldKey::Password,
            CopyField::Code => FieldKey::Code,
        };
        // Row -1: the open item.
        let id = if row < 0 {
            with_state(|s| s.open.as_ref().map(|o| o.view.id.clone())).flatten()
        } else {
            row_id(row)
        };
        if let Some(id) = id {
            let _ = tx.send(Command::Copy { id, key });
        }
    });

    let tx = commands.clone();
    ui.on_copy_field(move |index| {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let command = with_state(|s| {
            let open = s.open.as_ref()?;
            match open.keys.get(index)? {
                Some(key) => Some(Command::Copy {
                    id: open.view.id.clone(),
                    key: key.clone(),
                }),
                None => Some(Command::CopyText(
                    s.fields.row_data(index)?.value.to_string(),
                )),
            }
        });
        if let Some(Some(command)) = command {
            let _ = tx.send(command);
        }
    });

    let tx = commands.clone();
    ui.on_reveal_field(move |index| {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let command = with_state(|s| {
            let mut row = s.fields.row_data(index)?;
            if row.revealed {
                // Hide again, and forget the value.
                row.revealed = false;
                row.value = Default::default();
                s.fields.set_row_data(index, row);
                return None;
            }
            let open = s.open.as_ref()?;
            Some(Command::Reveal {
                id: open.view.id.clone(),
                index,
                key: open.keys.get(index)?.clone()?,
            })
        });
        if let Some(Some(command)) = command {
            let _ = tx.send(command);
        }
    });

    ui.on_open_website(move |row| {
        let uri = with_state(|s| {
            let item = *s.shown.get(usize::try_from(row).ok()?)?;
            s.all[item].uris.iter().find(|u| web_address(u)).cloned()
        })
        .flatten();
        if let Some(uri) = uri {
            open_website(&uri);
        }
    });

    ui.on_open_field(move |index| {
        let uri = with_state(|s| s.fields.row_data(usize::try_from(index).ok()?))
            .flatten()
            .filter(|f| f.kind == FieldKind::Website)
            .map(|f| f.value.to_string());
        if let Some(uri) = uri {
            open_website(&uri);
        }
    });

    ui.on_back(move || {
        close_item();
        vault(|v| v.set_focus_request(v.get_focus_request() + 1));
    });

    let tx = commands.clone();
    ui.on_unlock(move || {
        vault(|v| {
            let password = Zeroizing::new(v.get_password().to_string());
            if password.is_empty() || v.get_busy() {
                return;
            }
            v.set_password("".into());
            v.set_error(UnlockError::None);
            v.set_busy(true);
            let _ = tx.send(Command::Unlock(password.clone()));
        });
    });
    connect_editing(ui, commands);
}

/// The open item's id.
fn open_id() -> Option<String> {
    with_state(|s| s.open.as_ref().map(|o| o.view.id.clone())).flatten()
}

/// Creating, editing, favourites and the trash (main window).
fn connect_editing(ui: &Vault<'_>, commands: &mpsc::UnboundedSender<Command>) {
    let tx = commands.clone();
    ui.on_new_item(move |kind| {
        // In a folder or among the favourites, the new item starts there.
        let (folder, favorite) = with_state(|s| match &s.category {
            CategoryKey::Folder(name) => (Some(name.clone()), false),
            CategoryKey::Favorites => (None, true),
            _ => (None, false),
        })
        .unwrap_or_default();
        let _ = tx.send(Command::New {
            kind: kind.to_string(),
            folder,
            favorite,
        });
    });

    let tx = commands.clone();
    ui.on_edit_item(move || {
        if let Some(id) = open_id() {
            let _ = tx.send(Command::Edit(id));
        }
    });

    let tx = commands.clone();
    ui.on_manage(move |row, action| {
        // Row -1: the open item.
        let item = with_state(|s| {
            if row < 0 {
                let view = &s.open.as_ref()?.view;
                return Some((view.id.clone(), view.name.clone(), view.favorite));
            }
            let item = &s.all[*s.shown.get(usize::try_from(row).ok()?)?];
            Some((item.id.clone(), item.name.clone(), item.favorite))
        })
        .flatten();
        let Some((id, name, favorite)) = item else {
            return;
        };
        // The row's item opens too, so what happens is what the right column shows.
        if row >= 0 {
            end_edit();
            let _ = tx.send(Command::Open(id.clone()));
        }
        let command = match action.as_str() {
            "edit" => Command::Edit(id),
            "favorite" => Command::Favorite {
                id,
                favorite: !favorite,
            },
            "trash" => Command::Trash(id),
            "restore" => Command::Restore(id),
            "delete" => {
                // Remember which item the question is about: the open one may change
                // before the answer.
                with_state(|s| s.delete_target = Some(id));
                vault(|v| {
                    v.set_delete_name(name.as_str().into());
                    v.set_confirm_delete(true);
                });
                return;
            }
            _ => return,
        };
        let _ = tx.send(command);
    });

    ui.on_edit_value(|index, text| edit(|e| e.set_value(index as usize, &text)));
    ui.on_edit_uri(|index, text| edit(|e| e.set_uri(index as usize, &text)));
    ui.on_edit_field_name(|index, text| edit(|e| e.set_field_name(index as usize, &text)));
    ui.on_edit_field_value(|index, text| edit(|e| e.set_field_value(index as usize, &text)));
    ui.on_add_uri(|| {
        edit(Edit::add_uri);
        refill_form();
    });
    ui.on_remove_uri(|index| {
        edit(|e| e.remove_uri(index as usize));
        refill_form();
    });
    ui.on_add_field(|kind| {
        edit(|e| e.add_field(&kind));
        refill_form();
    });
    ui.on_remove_field(|index| {
        edit(|e| e.remove_field(index as usize));
        refill_form();
    });

    let tx = commands.clone();
    ui.on_generate_password(move |target| {
        vault(|v| {
            v.set_gen_target(target);
            v.set_gen_value("".into());
            v.set_gen_error("".into());
            v.set_gen_open(true);
            generate(v, &tx);
        });
    });

    let tx = commands.clone();
    ui.on_gen_refresh(move || vault(|v| generate(v, &tx)));

    let tx = commands.clone();
    ui.on_gen_copy(move || {
        vault(|v| {
            let value = v.get_gen_value().to_string();
            if !value.is_empty() {
                let _ = tx.send(Command::CopyText(value));
            }
        });
    });

    ui.on_gen_use(move || {
        vault(|v| {
            let value = v.get_gen_value().to_string();
            v.set_gen_open(false);
            v.set_gen_value("".into());
            if let Ok(index) = usize::try_from(v.get_gen_target()) {
                set_form_value(v, index, &value);
            }
        });
    });

    let tx = commands.clone();
    ui.on_import_key(move |index| {
        if let Ok(index) = usize::try_from(index) {
            let source = Source::ChooseKey;
            let _ = tx.send(Command::Fill { index, source });
        }
    });

    let tx = commands.clone();
    ui.on_scan_qr(move |index, screen| {
        if let Ok(index) = usize::try_from(index) {
            let source = if screen {
                Source::Screen
            } else {
                Source::ChooseImage
            };
            let _ = tx.send(Command::Fill { index, source });
        }
    });

    let tx = commands.clone();
    ui.on_save_edit(move || {
        let mut data = None;
        vault(|v| {
            if v.get_saving() {
                return;
            }
            let name = v.get_edit_name().to_string();
            let notes = v.get_edit_notes().to_string();
            let (favorite, reprompt, folder) = (
                v.get_edit_favorite(),
                v.get_edit_reprompt(),
                v.get_edit_folder(),
            );
            data = with_state(|s| {
                let edit = s.edit.as_mut()?;
                edit.data.name = name.clone();
                edit.data.notes = Zeroizing::new(notes.clone());
                edit.data.favorite = favorite;
                edit.data.reprompt = reprompt;
                edit.set_folder(folder);
                Some(edit.to_save())
            })
            .flatten();
            if data.is_some() {
                v.set_saving(true);
                v.set_edit_error("".into());
            }
        });
        if let Some(data) = data {
            let _ = tx.send(Command::Save(data));
        }
    });

    ui.on_cancel_edit(end_edit);

    let tx = commands.clone();
    ui.on_toggle_favorite(move || {
        let favorite = with_state(|s| s.open.as_ref().map(|o| o.view.favorite)).flatten();
        if let (Some(id), Some(favorite)) = (open_id(), favorite) {
            let _ = tx.send(Command::Favorite {
                id,
                favorite: !favorite,
            });
        }
    });

    let tx = commands.clone();
    ui.on_trash_item(move || {
        if let Some(id) = open_id() {
            let _ = tx.send(Command::Trash(id));
        }
    });

    let tx = commands.clone();
    ui.on_restore_item(move || {
        if let Some(id) = open_id() {
            let _ = tx.send(Command::Restore(id));
        }
    });

    let tx = commands.clone();
    ui.on_delete_item(move || {
        if let Some(Some(id)) = with_state(|s| s.delete_target.take()) {
            let _ = tx.send(Command::Delete(id));
        }
    });
}

/// Asks for a value with the generator's options. Nothing to ask without a kind of
/// character: the dialog says so.
fn generate(v: &Vault<'_>, tx: &mpsc::UnboundedSender<Command>) {
    let request = if v.get_gen_passphrase() {
        Request::GeneratePassphrase(PassphraseParams {
            words: v.get_gen_words().clamp(3, 20) as u8,
            separator: v.get_gen_separator().to_string(),
            capitalize: v.get_gen_capitalize(),
            include_number: v.get_gen_include_number(),
        })
    } else {
        let params = PasswordParams {
            length: v.get_gen_length().clamp(5, 128) as u8,
            uppercase: v.get_gen_upper(),
            lowercase: v.get_gen_lower(),
            numbers: v.get_gen_numbers(),
            special: v.get_gen_special(),
            avoid_ambiguous: v.get_gen_avoid_ambiguous(),
        };
        if !(params.uppercase || params.lowercase || params.numbers || params.special) {
            v.set_gen_value("".into());
            return;
        }
        Request::GeneratePassword(params)
    };
    let _ = tx.send(Command::Generate(request));
}

/// Puts a value into row `index` of the form, as if typed.
fn set_form_value(v: &Vault<'_>, index: usize, value: &str) {
    v.set_edit_problem(EditProblem::None);
    edit(|e| e.set_value(index, value));
    with_state(|s| {
        if let Some(mut row) = s.form.values.row_data(index) {
            row.value = value.into();
            s.form.values.set_row_data(index, row);
        }
    });
}

/// Reads a value for the form from `source` (on the worker thread: file dialogs and
/// screenshots wait for the user) and puts it in, or says why not.
async fn fill(post: &Post, index: usize, source: Source) {
    let chosen = |result: Result<PathBuf, Problem>, read: fn(&std::path::Path) -> _| {
        result.and_then(|path| read(&path))
    };
    let result = match source {
        Source::ChooseKey => chosen(scan::choose_file(false).await, scan::key_in_file),
        Source::KeyFile(path) => scan::key_in_file(&path),
        Source::Screen => scan::authenticator_on_screen().await,
        Source::ChooseImage => chosen(scan::choose_file(true).await, scan::authenticator_in_file),
        Source::ImageFile(path) => scan::authenticator_in_file(&path),
    };
    let problem = match result {
        Ok(value) => {
            let value = value.to_string();
            ui(post, move |ui| set_form_value(ui, index, &value));
            return;
        }
        Err(Problem::Cancelled) => return,
        Err(Problem::NoQrCode) => EditProblem::NoQrCode,
        Err(Problem::NotAKey) => EditProblem::NotAKey,
        Err(Problem::Unreadable(e)) => {
            tracing::warn!("cannot read the file: {e}");
            EditProblem::Unreadable
        }
        Err(Problem::NoScreenshot(e)) => {
            tracing::warn!("no screenshot: {e}");
            EditProblem::NoScreenshot
        }
    };
    ui(post, move |ui| ui.set_edit_problem(problem));
}

/// Changes the item in the form (what was typed).
fn edit(f: impl FnOnce(&mut Edit)) {
    with_state(|s| s.edit.as_mut().map(f));
}

/// Rebuilds the form's rows from the item, after rows were added or removed.
fn refill_form() {
    with_state(|s| {
        let Some(edit) = &s.edit else { return };
        s.form.values.set_vec(edit.values());
        update_model(&s.form.groups, edit.groups());
        s.form.uris.set_vec(edit.uris());
        s.form.fields.set_vec(edit.fields());
    });
}

/// Opens the form for `edit`.
fn start_edit(ui: &Vault<'_>, edit: Edit) {
    ui.set_edit_new(edit.data.id.is_none());
    ui.set_edit_kind(edit.data.kind.as_str().into());
    ui.set_edit_name(edit.data.name.as_str().into());
    ui.set_edit_notes(edit.data.notes.as_str().into());
    ui.set_edit_favorite(edit.data.favorite);
    ui.set_edit_reprompt(edit.data.reprompt);
    ui.set_edit_folder(edit.folder_index());
    ui.set_edit_error("".into());
    ui.set_edit_problem(EditProblem::None);
    ui.set_saving(false);
    with_state(|s| {
        s.form.folders.set_vec(edit.folder_names());
        s.edit = Some(edit);
    });
    refill_form();
    ui.set_editing(true);
}

/// Closes the form and forgets what was in it.
fn end_edit() {
    let was_editing = with_state(|s| s.edit.take().is_some()).unwrap_or_default();
    with_state(|s| {
        s.form.values.set_vec(Vec::new());
        s.form.groups.set_vec(Vec::new());
        s.form.uris.set_vec(Vec::new());
        s.form.fields.set_vec(Vec::new());
    });
    vault(|v| {
        v.set_editing(false);
        v.set_saving(false);
        v.set_edit_name("".into());
        v.set_edit_notes("".into());
        if was_editing {
            v.set_focus_request(v.get_focus_request() + 1);
        }
    });
}

/// Whether a stored URI is a website the browser can open (not an app id and the like).
fn web_address(uri: &str) -> bool {
    nibwarden_platform::browser::is_web(uri)
}

fn open_website(uri: &str) {
    if let Err(e) = nibwarden_platform::browser::open(uri) {
        let message = format!("cannot open {uri}: {e}");
        vault(|v| v.set_message(message.as_str().into()));
    }
}

/// The item id of a visible row.
fn row_id(row: i32) -> Option<String> {
    with_state(|s| {
        let item = *s.shown.get(usize::try_from(row).ok()?)?;
        Some(s.all[item].id.clone())
    })
    .flatten()
}

fn close_item() {
    with_state(|s| {
        s.open = None;
        s.fields.set_vec(Vec::new());
        s.groups.set_vec(Vec::new());
    });
    vault(|v| v.set_item_open(false));
}

/// New items from the daemon. The search, category and open item stay as they were, so
/// a refresh in the background does not disturb the user.
fn set_items(items: Vec<ItemInfo>) {
    let still_open = with_state(|s| {
        s.all = items;
        let mut keys = vec![CategoryKey::All];
        if s.all.iter().any(|i| i.favorite) {
            keys.push(CategoryKey::Favorites);
        }
        // Every kind, empty or not, as Bitwarden lists them.
        keys.extend(
            KINDS
                .iter()
                .map(|kind| CategoryKey::Kind((*kind).to_owned())),
        );
        let mut folders: Vec<&str> = s
            .all
            .iter()
            .filter(|i| !i.trashed)
            .filter_map(|i| i.folder.as_deref())
            .collect();
        folders.sort_unstable_by_key(|f| f.to_lowercase());
        folders.dedup();
        keys.extend(
            folders
                .into_iter()
                .map(|f| CategoryKey::Folder(f.to_owned())),
        );
        keys.push(CategoryKey::Trash);
        if !keys.contains(&s.category) {
            s.category = CategoryKey::All;
        }
        let rows: Vec<Category> = keys.iter().map(|key| category(key, &s.all)).collect();
        update_model(&s.categories, rows);
        s.category_keys = keys;
        let open = s.open.as_ref().map(|o| o.view.id.clone());
        open.is_none_or(|id| s.all.iter().any(|i| i.id == id))
    });
    let index = with_state(|s| s.category_keys.iter().position(|k| *k == s.category))
        .flatten()
        .unwrap_or(0);
    vault(|v| v.set_category(index as i32));
    if still_open == Some(false) {
        close_item();
    }
    refilter();
    follow_open();
}

/// The list row of the open item: -1 when the list does not show it; `None` with no
/// item open.
fn open_row() -> Option<i32> {
    with_state(|s| {
        let id = &s.open.as_ref()?.view.id;
        let row = s.shown.iter().position(|&i| s.all[i].id == *id);
        Some(row.map_or(-1, |row| row as i32))
    })
    .flatten()
}

/// Moves the selection to the open item, so the main window highlights the item it
/// shows, wherever the list put it.
fn follow_open() {
    if let Some(row) = open_row() {
        vault(|v| v.set_selected(row));
    }
}

fn category(key: &CategoryKey, all: &[ItemInfo]) -> Category {
    let (kind, label) = match key {
        CategoryKey::All => ("all", ""),
        CategoryKey::Favorites => ("favorites", ""),
        CategoryKey::Kind(kind) => (kind.as_str(), ""),
        CategoryKey::Folder(folder) => ("folder", folder.as_str()),
        CategoryKey::Trash => ("trash", ""),
    };
    Category {
        label: label.into(),
        kind: kind.into(),
        count: all.iter().filter(|i| key.matches(i)).count() as i32,
    }
}

fn refilter() {
    with_state(|s| {
        let candidates: Vec<usize> = (0..s.all.len())
            .filter(|&i| s.category.matches(&s.all[i]))
            .collect();
        let subset: Vec<ItemInfo> = candidates.iter().map(|&i| s.all[i].clone()).collect();
        s.shown = search::rank(&subset, &s.query)
            .into_iter()
            .map(|i| candidates[i])
            .collect();
        let rows: Vec<QuickItem> = s.shown.iter().map(|&i| quick_item(&s.all[i])).collect();
        update_model(&s.items, rows);
    });
}

/// Replaces a model's rows, notifying only the rows that changed. A background refresh
/// that brings nothing new must leave the rows alone: rebuilt rows lose their hover
/// highlight and close a context menu open on them.
fn update_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let old = model.row_count();
    for (index, row) in rows.iter().enumerate() {
        if index >= old {
            model.push(row.clone());
        } else if model.row_data(index).as_ref() != Some(row) {
            model.set_row_data(index, row.clone());
        }
    }
    for index in (rows.len()..old).rev() {
        model.remove(index);
    }
}

fn initial(name: &str) -> slint::SharedString {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default()
        .into()
}

fn quick_item(item: &ItemInfo) -> QuickItem {
    let login = item.kind == "login";
    QuickItem {
        kind: item.kind.as_str().into(),
        favorite: item.favorite,
        name: item.name.as_str().into(),
        subtitle: item.subtitle.as_str().into(),
        folder: item.folder.clone().unwrap_or_default().into(),
        initial: initial(&item.name),
        has_username: login && !item.subtitle.is_empty(),
        has_password: login,
        has_totp: item.has_totp,
        has_website: item.uris.iter().any(|u| web_address(u)),
        trashed: item.trashed,
    }
}

fn field(standard: FieldLabel, label: &str, kind: FieldKind, value: &str) -> ItemField {
    ItemField {
        standard,
        label: label.into(),
        value: value.into(),
        kind,
        revealed: false,
        remaining: 0,
        period: 0,
    }
}

/// The open item's rows, in cards: rows of one card are pushed together.
#[derive(Default)]
struct Rows {
    fields: Vec<ItemField>,
    keys: Vec<Option<FieldKey>>,
    groups: Vec<FieldGroup>,
}

impl Rows {
    fn push(&mut self, group: &str, field: ItemField, key: Option<FieldKey>) {
        match self.groups.last_mut() {
            Some(last) if last.title == group => last.count += 1,
            _ => self.groups.push(FieldGroup {
                title: group.into(),
                start: self.fields.len() as i32,
                count: 1,
            }),
        }
        self.fields.push(field);
        self.keys.push(key);
    }
}

/// An RFC 3339 time as local date and time; as given if it does not parse.
pub(crate) fn local_time(time: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(time).map_or_else(
        |_| time.to_owned(),
        |t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
}

/// Fills the item view: the fields an item has, secrets without their values. `notes`
/// were read already (they are shown as they are, unless the item asks for the master
/// password again).
fn show_item(ui: &Vault<'_>, view: ItemView, notes: Option<Zeroizing<String>>) {
    let mut rows = Rows::default();
    if let Some(username) = &view.username {
        let row = field(FieldLabel::Username, "", FieldKind::Text, username);
        rows.push("login", row, Some(FieldKey::Username));
    }
    if view.has_password {
        let row = field(FieldLabel::Password, "", FieldKind::Secret, "");
        rows.push("login", row, Some(FieldKey::Password));
    }
    if view.has_totp {
        let row = field(FieldLabel::Code, "", FieldKind::Code, "");
        rows.push("login", row, Some(FieldKey::Code));
    }
    for (key, value) in &view.values {
        let group = group_of(&view.kind, key);
        match value {
            Some(value) => {
                let row = field(FieldLabel::Value, key, FieldKind::Text, value);
                rows.push(group, row, None);
            }
            None => {
                // A private key is several lines: hidden like a password, shown like notes.
                let kind = if key == "private_key" {
                    FieldKind::Note
                } else {
                    FieldKind::Secret
                };
                let row = field(FieldLabel::Value, key, kind, "");
                rows.push(group, row, Some(FieldKey::Value(key.clone())));
            }
        }
    }
    for uri in &view.uris {
        let row = field(FieldLabel::Website, "", FieldKind::Website, uri);
        rows.push("websites", row, None);
    }
    if view.has_notes {
        match &notes {
            Some(notes) => {
                let mut row = field(FieldLabel::Notes, "", FieldKind::Multiline, notes);
                row.revealed = true;
                rows.push("notes", row, None);
            }
            None => {
                let row = field(FieldLabel::Notes, "", FieldKind::Note, "");
                rows.push("notes", row, Some(FieldKey::Notes));
            }
        }
    }
    for custom in &view.fields {
        match &custom.value {
            Some(value) => {
                let row = field(FieldLabel::Custom, &custom.name, FieldKind::Text, value);
                rows.push("custom", row, None);
            }
            None => {
                let row = field(FieldLabel::Custom, &custom.name, FieldKind::Secret, "");
                rows.push("custom", row, Some(FieldKey::Custom(custom.name.clone())));
            }
        }
    }
    if !view.revised.is_empty() {
        let row = field(
            FieldLabel::Revised,
            "",
            FieldKind::Info,
            &local_time(&view.revised),
        );
        rows.push("history", row, None);
    }
    if !view.created.is_empty() {
        let row = field(
            FieldLabel::Created,
            "",
            FieldKind::Info,
            &local_time(&view.created),
        );
        rows.push("history", row, None);
    }
    if let Some(time) = &view.password_revised {
        let row = field(
            FieldLabel::PasswordRevised,
            "",
            FieldKind::Info,
            &local_time(time),
        );
        rows.push("history", row, None);
    }
    // Earlier passwords, labelled with when they were replaced.
    for (index, time) in view.password_history.iter().enumerate() {
        let row = field(FieldLabel::Custom, &local_time(time), FieldKind::Secret, "");
        let key = FieldKey::Value(format!("password_history_{index}"));
        rows.push("password_history", row, Some(key));
    }
    ui.set_item_favorite(view.favorite);
    ui.set_item_trashed(view.trashed);
    ui.set_item_name(view.name.as_str().into());
    ui.set_item_folder(view.folder.clone().unwrap_or_default().into());
    ui.set_item_initial(initial(&view.name));
    with_state(|s| {
        s.fields.set_vec(rows.fields);
        update_model(&s.groups, rows.groups);
        s.open = Some(OpenItem {
            view,
            keys: rows.keys,
        });
    });
    if let Some(row) = open_row() {
        ui.set_selected(row);
    }
    ui.set_item_open(true);
}

/// Counts the open item's one-time code down; asks for a new one when it runs out (or
/// was never fetched). Items that ask for the master password again are left alone: a
/// code would only appear after a prompt the user did not ask for.
fn tick_codes(tx: &mpsc::UnboundedSender<Command>) {
    with_state(|s| {
        let Some(open) = &s.open else { return };
        if open.view.reprompt {
            return;
        }
        for index in 0..s.fields.row_count() {
            let Some(mut row) = s.fields.row_data(index) else {
                continue;
            };
            if row.kind != FieldKind::Code {
                continue;
            }
            if row.remaining > 1 {
                row.remaining -= 1;
                s.fields.set_row_data(index, row);
            } else if row.remaining != -1 {
                // -1: a refresh is on its way.
                row.remaining = -1;
                s.fields.set_row_data(index, row);
                let _ = tx.send(Command::Code {
                    id: open.view.id.clone(),
                    index,
                });
            }
        }
    });
}

/// Updates the window from the worker thread.
fn ui(post: &Post, update: impl FnOnce(&Vault<'_>) + Send + 'static) {
    post(Box::new(update));
}

fn show_notice(post: &Post, notice: QuickNotice) {
    ui(post, move |ui| {
        ui.set_notice(notice);
        ui.set_state(QuickState::Message);
    });
}

fn show_message(post: &Post, message: String) {
    ui(post, move |ui| {
        ui.set_notice(QuickNotice::None);
        ui.set_message(message.into());
        ui.set_state(QuickState::Message);
    });
}

/// A problem with one action, shown under the list without leaving it.
fn show_problem(post: &Post, message: String) {
    ui(post, move |ui| ui.set_message(message.into()));
}

fn show_toast(post: &Post, copied: Copied) {
    ui(post, move |ui| {
        ui.set_message("".into());
        ui.set_copied(copied);
        ui.set_toast_shown(true);
        let generation = TOAST_GENERATION.with(|g| {
            g.set(g.get() + 1);
            g.get()
        });
        slint::Timer::single_shot(TOAST, move || {
            if TOAST_GENERATION.with(Cell::get) == generation {
                vault(|v| v.set_toast_shown(false));
            }
        });
    });
}

fn set_field(post: &Post, index: usize, update: impl FnOnce(&mut ItemField) + Send + 'static) {
    ui(post, move |_| {
        with_state(|s| {
            if let Some(mut row) = s.fields.row_data(index) {
                update(&mut row);
                s.fields.set_row_data(index, row);
            }
        });
    });
}

/// The copied secret and when to clear it (`None`: when something else replaces it).
struct Pending {
    value: Zeroizing<String>,
    clear_at: Option<tokio::time::Instant>,
}

fn worker(
    mut commands: mpsc::UnboundedReceiver<Command>,
    post: Post,
    clear_after: Option<Duration>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            return;
        }
    };
    runtime.block_on(async move {
        let mut client = match connect().await {
            Ok(client) => client,
            Err(()) => {
                show_notice(&post, QuickNotice::NoDaemon);
                // Still drain commands until the window closes.
                while let Some(command) = commands.recv().await {
                    if matches!(command, Command::Quit) {
                        break;
                    }
                }
                return;
            }
        };
        let mut clipboard: Option<Clipboard> = None;
        let mut pending: Option<Pending> = None;
        let mut quitting = false;
        loop {
            // Wake up to clear the clipboard, or to see whether it is still ours.
            let wake = match &pending {
                Some(Pending { clear_at: Some(at), .. }) => Some(*at),
                Some(Pending { clear_at: None, .. }) => Some(tokio::time::Instant::now() + STILL_OURS),
                None => None,
            };
            let wake = async {
                match wake {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = commands.recv(), if !quitting => {
                    let Some(command) = command else {
                        quitting = true;
                        if pending.is_none() { break; }
                        continue;
                    };
                    let copied = match command {
                        Command::Quit => {
                            quitting = true;
                            if pending.is_none() { break; }
                            continue;
                        }
                        Command::Load => {
                            load(&mut client, &post).await;
                            continue;
                        }
                        Command::Refresh => {
                            refresh(&mut client, &post).await;
                            continue;
                        }
                        Command::Lock => {
                            if let Err(e) = client.call::<()>(Request::Lock).await {
                                show_problem(&post, e.to_string());
                            }
                            load(&mut client, &post).await;
                            continue;
                        }
                        Command::SignOut => {
                            if let Err(e) = client.call::<()>(Request::Logout).await {
                                show_problem(&post, e.to_string());
                            }
                            load(&mut client, &post).await;
                            continue;
                        }
                        Command::Sync => {
                            match client.call::<SyncResult>(Request::Sync { force: false }).await {
                                Ok(_) => refresh(&mut client, &post).await,
                                Err(e) => problem(&mut client, &post, e).await,
                            }
                            ui(&post, |ui| ui.set_syncing(false));
                            continue;
                        }
                        Command::Unlock(password) => {
                            match client.call::<()>(Request::Unlock { password }).await {
                                Ok(()) => load(&mut client, &post).await,
                                Err(ClientError::Remote(e)) if e.code == ErrorCode::Denied => ui(&post, |ui| {
                                    ui.set_busy(false);
                                    ui.set_error(UnlockError::WrongPassword);
                                }),
                                Err(e) => show_message(&post, e.to_string()),
                            }
                            continue;
                        }
                        Command::Open(id) => {
                            open_item(&mut client, &post, id).await;
                            continue;
                        }
                        command @ (Command::Edit(_)
                        | Command::New { .. }
                        | Command::Save(_)
                        | Command::Favorite { .. }
                        | Command::Trash(_)
                        | Command::Restore(_)
                        | Command::Delete(_)) => {
                            change(&mut client, &post, command).await;
                            continue;
                        }
                        Command::Fill { index, source } => {
                            fill(&post, index, source).await;
                            continue;
                        }
                        Command::Generate(request) => {
                            let result = client.call::<ValueResult>(request).await;
                            ui(&post, move |ui| match result {
                                Ok(generated) => {
                                    ui.set_gen_error("".into());
                                    ui.set_gen_value(generated.value.as_str().into());
                                }
                                Err(e) => ui.set_gen_error(e.to_string().into()),
                            });
                            continue;
                        }
                        Command::Reveal { id, index, key } => {
                            match read(&mut client, id, &key).await {
                                Ok(value) => {
                                    let value = value.to_string();
                                    set_field(&post, index, move |row| {
                                        row.value = value.into();
                                        row.revealed = true;
                                    });
                                }
                                Err(e) => problem(&mut client, &post, e).await,
                            }
                            continue;
                        }
                        Command::Code { id, index } => {
                            let request = Request::ItemTotp { folder: None, item: id };
                            if let Ok(totp) = client.call::<TotpResult>(request).await {
                                let code = group_code(&totp.code);
                                let remaining = totp.remaining as i32;
                                let period = totp.period as i32;
                                set_field(&post, index, move |row| {
                                    row.value = code.into();
                                    row.remaining = remaining;
                                    row.period = period;
                                });
                            }
                            continue;
                        }
                        Command::CopyText(text) => (Zeroizing::new(text), Copied::Other),
                        Command::CopyAs(value, what) => (value, what),
                        Command::Copy { id, key } => {
                            let copied = match key {
                                FieldKey::Username => Copied::Username,
                                FieldKey::Password => Copied::Password,
                                FieldKey::Code => Copied::Code,
                                _ => Copied::Other,
                            };
                            match read(&mut client, id, &key).await {
                                Ok(value) => (value, copied),
                                Err(e) => {
                                    problem(&mut client, &post, e).await;
                                    continue;
                                }
                            }
                        }
                    };
                    let (value, what) = copied;
                    if clipboard.is_none() {
                        clipboard = Clipboard::open().ok();
                    }
                    let result = match clipboard.as_mut() {
                        Some(clipboard) => clipboard.set_secret(&value),
                        None => Err(std::io::Error::other("no clipboard")),
                    };
                    match result {
                        Ok(()) => {
                            pending = Some(Pending {
                                value,
                                clear_at: clear_after.map(|d| tokio::time::Instant::now() + d),
                            });
                            show_toast(&post, what);
                        }
                        Err(e) => show_problem(&post, format!("cannot use the clipboard: {e}")),
                    }
                }
                () = wake => {
                    if let (Some(clipboard), Some(current)) = (clipboard.as_mut(), pending.as_ref()) {
                        let due = current.clear_at.is_some_and(|at| at <= tokio::time::Instant::now());
                        if due {
                            let _ = clipboard.clear_if_still(&current.value);
                            pending = None;
                        } else if !clipboard.still_holds(&current.value) {
                            // Replaced by something else: nothing left to serve or clear.
                            pending = None;
                        }
                    }
                    if quitting && pending.is_none() {
                        break;
                    }
                }
            }
        }
    });
}

/// `123456` → `123 456`, as authenticator apps show it.
fn group_code(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

/// Reacts to a failed action: a vault that locked meanwhile shows the unlock form, the
/// rest is shown under the list.
async fn problem(client: &mut Client, post: &Post, error: ClientError) {
    match &error {
        ClientError::Remote(e) if e.code == ErrorCode::Locked => load(client, post).await,
        _ => show_problem(post, error.to_string()),
    }
}

pub(crate) async fn connect() -> Result<Client, ()> {
    let socket = std::env::var_os("NIBWARDEN_SOCKET")
        .map(PathBuf::from)
        .or_else(nibwarden_ipc::default_socket_path)
        .ok_or(())?;
    if let Ok(client) = Client::connect(&socket, "nibwarden-gui").await {
        return Ok(client);
    }
    if nibwarden_platform::service::start_daemon().await {
        for _ in 0..50 {
            if let Ok(client) = Client::connect(&socket, "nibwarden-gui").await {
                return Ok(client);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Err(())
}

/// Shows whatever state the vault is in: the list, the unlock form, or why neither.
async fn load(client: &mut Client, post: &Post) {
    let status: StatusResult = match client.call(Request::Status).await {
        Ok(status) => status,
        Err(e) => return show_message(post, e.to_string()),
    };
    if let Some(email) = status.email.clone() {
        ui(post, move |ui| {
            ui.set_initial(initial(&email));
            ui.set_email(email.into());
        });
    }
    match status.state {
        LockState::LoggedOut => show_notice(post, QuickNotice::LoggedOut),
        LockState::Locked => {
            ui(post, move |ui| {
                end_edit();
                close_item();
                ui.set_busy(false);
                ui.set_state(QuickState::Locked);
            });
        }
        LockState::Unlocked => match fetch_items(client).await {
            Ok(items) => ui(post, move |ui| {
                set_items(items);
                ui.set_selected(0);
                ui.set_busy(false);
                ui.set_state(QuickState::List);
                ui.set_focus_request(ui.get_focus_request() + 1);
            }),
            Err(e) => show_message(post, e.to_string()),
        },
    }
}

/// Follows the daemon without disturbing the window: a vault that locked shows the unlock
/// form, one that was unlocked elsewhere shows the list, and an unlocked vault's items are
/// replaced in place.
async fn refresh(client: &mut Client, post: &Post) {
    let Ok(status) = client.call::<StatusResult>(Request::Status).await else {
        return;
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    ui(post, move |ui| {
        let _ = tx.send(ui.get_state());
    });
    let shown = rx.await.unwrap_or(QuickState::Loading);
    match (status.state, shown) {
        (LockState::Unlocked, QuickState::List) => {
            if let Ok(items) = fetch_items(client).await {
                ui(post, move |_| set_items(items));
            }
        }
        // Typing a password, or already showing why there is nothing to show.
        (LockState::Locked, QuickState::Locked) | (LockState::LoggedOut, QuickState::Message) => {}
        _ => load(client, post).await,
    }
}

/// The live items and the trash.
async fn fetch_items(client: &mut Client) -> Result<Vec<ItemInfo>, ClientError> {
    let mut items = Vec::new();
    for trash in [false, true] {
        let request = Request::ItemList {
            search: None,
            folder: None,
            trash,
        };
        let list: ItemListResult = client.call(request).await?;
        items.extend(list.items.into_iter().map(|mut i| {
            i.trashed = trash;
            i
        }));
    }
    Ok(items)
}

async fn open_item(client: &mut Client, post: &Post, id: String) {
    let request = Request::ItemView {
        folder: None,
        item: id,
    };
    match client.call::<ItemView>(request).await {
        Ok(view) => {
            // Notes are shown as they are, as Bitwarden and 1Password do.
            let notes = if view.has_notes && !view.reprompt {
                read(client, view.id.clone(), &FieldKey::Notes).await.ok()
            } else {
                None
            };
            ui(post, move |ui| show_item(ui, view, notes));
        }
        Err(e) => problem(client, post, e).await,
    }
}

/// After a change: the list again, and the changed item (`None`: it is gone).
async fn after_change(client: &mut Client, post: &Post, open: Option<String>) {
    if let Ok(items) = fetch_items(client).await {
        ui(post, move |_| set_items(items));
    }
    match open {
        Some(id) => open_item(client, post, id).await,
        None => ui(post, |_| close_item()),
    }
}

async fn folders(client: &mut Client) -> Result<Vec<nibwarden_ipc::FolderEntry>, ClientError> {
    Ok(client
        .call::<FolderListResult>(Request::FolderList)
        .await?
        .folders)
}

/// Runs one change to the vault.
async fn change(client: &mut Client, post: &Post, command: Command) {
    match command {
        Command::Edit(id) => {
            let content = async {
                let folders = folders(client).await?;
                let data: ItemData = client.call(Request::ItemContent { id }).await?;
                Ok::<_, ClientError>(Edit::new(data, folders))
            };
            match content.await {
                Ok(edit) => ui(post, move |ui| start_edit(ui, edit)),
                Err(e) => problem(client, post, e).await,
            }
        }
        Command::New {
            kind,
            folder,
            favorite,
        } => match folders(client).await {
            Ok(folders) => {
                let folder_id = folder.and_then(|name| {
                    folders
                        .iter()
                        .find(|f| f.name == name)
                        .map(|f| f.id.clone())
                });
                let data = ItemData {
                    kind,
                    folder_id,
                    favorite,
                    ..Default::default()
                };
                let edit = Edit::new(data, folders);
                ui(post, move |ui| start_edit(ui, edit));
            }
            Err(e) => problem(client, post, e).await,
        },
        Command::Save(item) => match client.call::<SavedResult>(Request::ItemSave { item }).await {
            Ok(saved) => {
                ui(post, |_| end_edit());
                after_change(client, post, Some(saved.id)).await;
            }
            Err(e) => {
                let message = e.to_string();
                ui(post, move |ui| {
                    ui.set_saving(false);
                    ui.set_edit_error(message.into());
                });
            }
        },
        Command::Favorite { id, favorite } => {
            match client
                .call::<()>(Request::ItemFavorite {
                    id: id.clone(),
                    favorite,
                })
                .await
            {
                Ok(()) => after_change(client, post, Some(id)).await,
                Err(e) => problem(client, post, e).await,
            }
        }
        Command::Trash(id) => match client.call::<()>(Request::ItemTrash { id }).await {
            Ok(()) => after_change(client, post, None).await,
            Err(e) => problem(client, post, e).await,
        },
        Command::Restore(id) => {
            match client
                .call::<()>(Request::ItemRestore { id: id.clone() })
                .await
            {
                Ok(()) => after_change(client, post, Some(id)).await,
                Err(e) => problem(client, post, e).await,
            }
        }
        Command::Delete(id) => match client.call::<()>(Request::ItemDelete { id }).await {
            Ok(()) => after_change(client, post, None).await,
            Err(e) => problem(client, post, e).await,
        },
        _ => unreachable!("not a change"),
    }
}

async fn read(
    client: &mut Client,
    id: String,
    key: &FieldKey,
) -> Result<Zeroizing<String>, ClientError> {
    let field = match key {
        FieldKey::Code => {
            let totp: TotpResult = client
                .call(Request::ItemTotp {
                    folder: None,
                    item: id,
                })
                .await?;
            return Ok(totp.code);
        }
        FieldKey::Username => "username".to_owned(),
        FieldKey::Password => "password".to_owned(),
        FieldKey::Notes => "notes".to_owned(),
        FieldKey::Custom(name) | FieldKey::Value(name) => name.clone(),
    };
    let value: ValueResult = client
        .call(Request::ItemGet {
            folder: None,
            item: id,
            field,
        })
        .await?;
    Ok(value.value)
}

#[cfg(test)]
mod tests {
    use super::group_code;

    #[test]
    fn groups_six_digit_codes() {
        assert_eq!(group_code("123456"), "123 456");
        assert_eq!(group_code("12345678"), "12345678");
    }
}
