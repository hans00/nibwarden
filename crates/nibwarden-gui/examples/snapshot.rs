//! Renders nibwarden windows to PNG without a display, for reviewing the design:
//!
//!     cargo run -p nibwarden-gui --example snapshot -- <out-dir>
//!
//! Writes `<window>-<lang>-<light|dark>.png` at 2× scale for every window and mode.

use std::{fs::File, io::BufWriter, path::Path, rc::Rc};

use nibwarden_gui::{
    Account, AccountPage, ApprovalScope, ApproveWindow, Category, Copied, EditCustom, EditProblem,
    EditValue, FieldGroup, FieldKind, FieldLabel, ItemField, QuickItem, QuickNotice, QuickState,
    QuickWindow, SendDetail, SendRow, Sends, SettingsWindow, Theme, Transfer, UnlockError,
    UnlockMode, UnlockWindow, Vault, VaultWindow,
    prompt::{show_access_approve, show_access_unlock, show_unlock},
};
use nibwarden_ipc::prompt::{Access, Action, BrowserUse, Requester, Scope, SignPurpose};
use slint::{
    ComponentHandle, Global, ModelRc, PhysicalSize, VecModel,
    platform::{
        Platform, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType},
    },
};

const SCALE: f32 = 2.0;
// Window widths include the transparent margin for the shadow (Theme.window-margin).
const WIDTH: f32 = 420.0 + 24.0;

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn requester(name: &str, pid: i32) -> Requester {
    Requester {
        name: name.into(),
        path: Some(format!("/usr/bin/{name}")),
        pid: Some(pid),
    }
}

fn main() {
    let out = std::env::args().nth(1).expect("usage: snapshot <out-dir>");
    let out = Path::new(&out);
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: SCALE,
    });

    let waiting = [requester("ssh", 48213), requester("git", 48220)];
    let git_sign = Access {
        requester: requester("ssh-keygen", 48230),
        action: Action::SshSign {
            key: "Work laptop".into(),
            purpose: SignPurpose::Git,
        },
    };
    let ssh_login = Access {
        requester: requester("ssh", 48213),
        action: Action::SshSign {
            key: "Work laptop".into(),
            purpose: SignPurpose::Login {
                user: "deploy".into(),
            },
        },
    };
    let read = Access {
        requester: requester("nibwarden", 48300),
        action: Action::ReadSecret {
            item: "Bank".into(),
            field: "password".into(),
        },
    };
    let import = Access {
        requester: requester("nibwarden", 48310),
        action: Action::Import {
            source: "bitwarden_export.json".into(),
            items: Some(42),
        },
    };
    let browser = Access {
        requester: requester("firefox", 2210),
        action: Action::Browser {
            what: BrowserUse::Unlock,
        },
    };
    let email = "hans@example.com";
    let terminal = requester("gnome-terminal-server", 3120);
    let firefox = requester("firefox", 2210);

    for lang in ["en", "zh_TW", "zh_CN"] {
        for dark in [false, true] {
            let theme = if dark { "dark" } else { "light" };
            let name = |window: &str| out.join(format!("{window}-{lang}-{theme}.png"));

            let ui = UnlockWindow::new().unwrap();
            setup(&ui, lang, dark);
            show_unlock(&ui, email, &waiting);
            render(
                &window,
                &ui,
                WIDTH,
                |ui| ui.get_natural_height(),
                &name("unlock"),
            );

            let ui = UnlockWindow::new().unwrap();
            setup(&ui, lang, dark);
            show_unlock(&ui, email, &[]);
            ui.set_password("hunter2".into());
            ui.set_error(UnlockError::WrongPassword);
            render(
                &window,
                &ui,
                WIDTH,
                |ui| ui.get_natural_height(),
                &name("unlock-error"),
            );

            let ui = UnlockWindow::new().unwrap();
            setup(&ui, lang, dark);
            show_unlock(&ui, email, &[]);
            ui.set_mode(UnlockMode::UnlockAllow);
            show_access_unlock(&ui, &git_sign);
            ui.set_scope(ApprovalScope::Application);
            ui.set_app("gnome-terminal-server".into());
            render(
                &window,
                &ui,
                WIDTH,
                |ui| ui.get_natural_height(),
                &name("unlock-allow"),
            );

            let ui = UnlockWindow::new().unwrap();
            setup(&ui, lang, dark);
            show_unlock(&ui, email, &[]);
            ui.set_mode(UnlockMode::Reprompt);
            show_access_unlock(&ui, &read);
            render(
                &window,
                &ui,
                WIDTH,
                |ui| ui.get_natural_height(),
                &name("reprompt"),
            );

            let ui = ApproveWindow::new().unwrap();
            setup(&ui, lang, dark);
            show_access_approve(&ui, &ssh_login, Some(&terminal), Scope::Application);
            render(
                &window,
                &ui,
                WIDTH,
                |ui| ui.get_natural_height(),
                &name("approve-ssh"),
            );

            let items = || {
                ModelRc::new(VecModel::from(vec![
                    quick_item("GitHub", "alice", "Work 工作"),
                    quick_item("GitLab", "alice@example.com", ""),
                    quick_item("Gitea (home)", "admin", "Home"),
                    quick_item("Legit Bank", "alice", ""),
                ]))
            };
            let field = |standard, label: &str, kind, value: &str, revealed, remaining| ItemField {
                standard,
                label: label.into(),
                value: value.into(),
                kind,
                revealed,
                remaining,
                period: 30,
            };
            let fields = || {
                ModelRc::new(VecModel::from(vec![
                    field(FieldLabel::Username, "", FieldKind::Text, "alice", false, 0),
                    field(FieldLabel::Password, "", FieldKind::Secret, "", false, 0),
                    field(FieldLabel::Code, "", FieldKind::Code, "482 913", false, 17),
                    field(
                        FieldLabel::Website,
                        "",
                        FieldKind::Website,
                        "https://github.com",
                        false,
                        0,
                    ),
                    field(
                        FieldLabel::Website,
                        "",
                        FieldKind::Website,
                        "https://gist.github.com/alice",
                        false,
                        0,
                    ),
                    field(
                        FieldLabel::Notes,
                        "",
                        FieldKind::Multiline,
                        "Recovery codes are in the safe.\nSecond line of notes.",
                        true,
                        0,
                    ),
                    field(
                        FieldLabel::Custom,
                        "API key",
                        FieldKind::Secret,
                        "",
                        false,
                        0,
                    ),
                    field(
                        FieldLabel::Revised,
                        "",
                        FieldKind::Info,
                        "2026-11-04 09:12",
                        false,
                        0,
                    ),
                    field(
                        FieldLabel::Created,
                        "",
                        FieldKind::Info,
                        "2025-03-01 18:40",
                        false,
                        0,
                    ),
                    field(
                        FieldLabel::Custom,
                        "2026-10-30 21:05",
                        FieldKind::Secret,
                        "",
                        false,
                        0,
                    ),
                ]))
            };
            let group = |title: &str, start, count| FieldGroup {
                title: title.into(),
                start,
                count,
            };
            let groups = || {
                ModelRc::new(VecModel::from(vec![
                    group("login", 0, 3),
                    group("websites", 3, 2),
                    group("notes", 5, 1),
                    group("custom", 6, 1),
                    group("history", 7, 2),
                    group("password_history", 9, 1),
                ]))
            };
            let open_item = |vault: &Vault| {
                vault.set_item_open(true);
                vault.set_item_name("GitHub".into());
                vault.set_item_folder("Work 工作".into());
                vault.set_item_initial("G".into());
                vault.set_fields(fields());
                vault.set_groups(groups());
            };

            let quick = |state: QuickState| {
                let ui = QuickWindow::new().unwrap();
                setup(&ui, lang, dark);
                let vault = ui.global::<Vault>();
                vault.set_state(state);
                vault.set_email(email.into());
                vault.set_initial("H".into());
                ui
            };
            let ui = quick(QuickState::List);
            let vault = ui.global::<Vault>();
            vault.set_query("git".into());
            vault.set_items(items());
            vault.set_selected(0);
            render(&window, &ui, 624.0, |_| 504.0, &name("quick-list"));

            let ui = quick(QuickState::List);
            let vault = ui.global::<Vault>();
            open_item(&vault);
            vault.set_copied(Copied::Password);
            vault.set_toast_shown(true);
            render(&window, &ui, 624.0, |_| 504.0, &name("quick-item"));

            let ui = quick(QuickState::Locked);
            ui.global::<Vault>().set_busy(true);
            render(&window, &ui, 624.0, |_| 504.0, &name("quick-unlocking"));
            // The spinner must move on its own: a later frame differs.
            std::thread::sleep(std::time::Duration::from_millis(300));
            render(
                &window,
                &ui,
                624.0,
                |_| 504.0,
                &name("quick-unlocking-later"),
            );
            let frame = |n: &str| std::fs::read(name(n)).unwrap();
            assert_ne!(
                frame("quick-unlocking"),
                frame("quick-unlocking-later"),
                "spinner is static"
            );

            let main = |state: QuickState| {
                let ui = VaultWindow::new().unwrap();
                setup(&ui, lang, dark);
                let vault = ui.global::<Vault>();
                vault.set_state(state);
                vault.set_email(email.into());
                vault.set_initial("H".into());
                ui
            };
            let ui = main(QuickState::List);
            let vault = ui.global::<Vault>();
            let category = |kind: &str, label: &str, count| Category {
                kind: kind.into(),
                label: label.into(),
                count,
            };
            vault.set_categories(ModelRc::new(VecModel::from(vec![
                category("all", "", 42),
                category("favorites", "", 5),
                category("login", "", 36),
                category("card", "", 2),
                category("note", "", 3),
                category("ssh_key", "", 1),
                category("folder", "Home", 12),
                category("folder", "Work 工作", 18),
            ])));
            vault.set_category(0);
            vault.set_items(items());
            vault.set_selected(0);
            open_item(&vault);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault"));
            // More items than fit: the list shows its scroll bar.
            let many: Vec<_> = (1..=30)
                .map(|i| quick_item(&format!("Site {i:02}"), "alice", ""))
                .collect();
            vault.set_items(ModelRc::new(VecModel::from(many)));
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-long-list"));
            vault.set_items(items());

            // Editing a login: the form takes the item's place.
            let edit_value = |key: &str, value: &str, secret: bool| EditValue {
                key: key.into(),
                value: value.into(),
                secret,
                multiline: false,
                readonly: false,
            };
            vault.set_editing(true);
            vault.set_edit_kind("login".into());
            vault.set_edit_name("GitHub".into());
            vault.set_edit_values(ModelRc::new(VecModel::from(vec![
                edit_value("username", "alice", false),
                edit_value("password", "correct horse battery", true),
                edit_value("totp", "", true),
            ])));
            vault.set_edit_groups(ModelRc::new(VecModel::from(vec![group("login", 0, 3)])));
            vault.set_edit_uris(ModelRc::new(VecModel::from(vec![
                slint::SharedString::from("https://github.com"),
                slint::SharedString::from(""),
            ])));
            vault.set_edit_fields(ModelRc::new(VecModel::from(vec![EditCustom {
                name: "API key".into(),
                value: "x".into(),
                kind: "hidden".into(),
            }])));
            vault.set_folders(ModelRc::new(VecModel::from(vec![
                slint::SharedString::new(),
                slint::SharedString::from("Work 工作"),
            ])));
            vault.set_edit_folder(1);
            vault.set_edit_favorite(true);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-edit"));

            // A new card, and the question before deleting for good.
            let ui = main(QuickState::List);
            let vault = ui.global::<Vault>();
            vault.set_items(items());
            vault.set_editing(true);
            vault.set_edit_new(true);
            vault.set_edit_kind("card".into());
            vault.set_edit_values(ModelRc::new(VecModel::from(
                [
                    "cardholder_name",
                    "brand",
                    "number",
                    "exp_month",
                    "exp_year",
                    "code",
                ]
                .iter()
                .map(|k| edit_value(k, "", *k == "number" || *k == "code"))
                .collect::<Vec<_>>(),
            )));
            vault.set_folders(ModelRc::new(VecModel::from(vec![
                slint::SharedString::new(),
            ])));
            vault.set_edit_groups(ModelRc::new(VecModel::from(vec![group("card", 0, 6)])));
            vault.set_edit_problem(EditProblem::NoQrCode);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-new-card"));

            // The password generator, over the form.
            vault.set_edit_problem(EditProblem::None);
            vault.set_gen_target(2);
            vault.set_gen_value("x7#Kq9!vR2m@Lp4&Zt8w".into());
            vault.set_gen_open(true);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-generator"));
            vault.set_gen_passphrase(true);
            vault.set_gen_value("crumpet-orbit-lantern-velvet".into());
            render(
                &window,
                &ui,
                1024.0,
                |_| 684.0,
                &name("vault-generator-passphrase"),
            );

            let ui = main(QuickState::List);
            let vault = ui.global::<Vault>();
            vault.set_items(items());
            open_item(&vault);
            vault.set_item_trashed(true);
            vault.set_delete_name("GitHub".into());
            vault.set_confirm_delete(true);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-delete"));

            // Sends: the list, one Send with its link, and the form for a new one.
            let ui = main(QuickState::List);
            ui.global::<Vault>()
                .set_categories(ModelRc::new(VecModel::from(vec![
                    category("all", "", 42),
                    category("trash", "", 1),
                ])));
            let sends = ui.global::<Sends>();
            let row = |name: &str, file: bool, deletion: &str| SendRow {
                name: name.into(),
                file,
                deletion: deletion.into(),
                disabled: false,
            };
            sends.set_rows(ModelRc::new(VecModel::from(vec![
                row("Wi-Fi password", false, "2026-11-11"),
                row("Contract draft.pdf", true, "2026-11-06"),
                row("Server notes 伺服器", false, "2026-12-04"),
            ])));
            sends.set_shown(true);
            sends.set_selected(0);
            sends.set_open(true);
            sends.set_detail(SendDetail {
                name: "Wi-Fi password".into(),
                text: "network: home-5G\npassword: correct horse battery staple".into(),
                hidden: true,
                link: "https://vault.bitwarden.com/#/send/q2xKBM9pT0e8kwbz1nQh4A/mX3vU1yQ0kQ7Z2cJr4hFPA"
                    .into(),
                deletion: "2026-11-11 09:30".into(),
                accesses: 1,
                max_accesses: 3,
                protected: true,
                notes: "For the guests on Saturday".into(),
                ..Default::default()
            });
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-send"));
            sends.set_editing(true);
            sends.set_form_file(true);
            sends.set_form_name("Contract draft".into());
            sends.set_form_file_name("contract-v3.pdf".into());
            sends.set_form_max_accesses("5".into());
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-send-new"));

            // Export, with its warning for an unencrypted file, and import of a protected file.
            let ui = main(QuickState::List);
            let vault = ui.global::<Vault>();
            vault.set_items(items());
            open_item(&vault);
            let transfer = ui.global::<Transfer>();
            transfer.set_export_open(true);
            transfer.set_format(1);
            transfer.set_master_password("secret".into());
            transfer.set_error(transfer.invoke_message("wrong-master".into()));
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-export"));
            transfer.set_export_open(false);
            transfer.set_error("".into());
            transfer.set_import_open(true);
            transfer.set_import_name("bitwarden_export_20261105.json".into());
            transfer.set_needs_password(true);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-import"));
            transfer.set_result(transfer.invoke_imported(42, 3));
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-imported"));

            let ui = main(QuickState::Locked);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-locked"));

            // A new account: nothing in the vault yet.
            let ui = main(QuickState::List);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-empty"));

            // Signed out: sign in, a two-step code, and the steps of a new account.
            let signed_out = |page: AccountPage| {
                let ui = main(QuickState::Message);
                ui.global::<Vault>().set_notice(QuickNotice::LoggedOut);
                let account = ui.global::<Account>();
                account.set_page(page);
                account.set_email(email.into());
                // As account.rs answers them; the snapshot only shows what was set.
                account.on_clean_code(|text| text);
                account.on_digit(|code, index| {
                    code.chars()
                        .nth(index as usize)
                        .map(String::from)
                        .unwrap_or_default()
                        .into()
                });
                ui
            };
            let ui = signed_out(AccountPage::SignIn);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-sign-in"));
            let account = ui.global::<Account>();
            account.set_server_kind(2);
            account.set_server_url("https://vault.example.com".into());
            account.set_password("secret".into());
            account.set_error("Username or password is incorrect. Try again.".into());
            render(
                &window,
                &ui,
                1024.0,
                |_| 684.0,
                &name("vault-sign-in-error"),
            );
            let ui = signed_out(AccountPage::TwoStep);
            let account = ui.global::<Account>();
            account.set_can_authenticator(true);
            account.set_can_email(true);
            account.set_use_email(true);
            account.set_code_sent(true);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-two-step"));
            let ui = signed_out(AccountPage::TwoStep);
            let account = ui.global::<Account>();
            account.set_new_device(true);
            account.set_use_email(true);
            account.set_code_sent(true);
            account.set_code("4821".into());
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-new-device"));
            account.set_code("482193".into());
            account.set_busy(true);
            render(
                &window,
                &ui,
                1024.0,
                |_| 684.0,
                &name("vault-new-device-busy"),
            );
            let ui = signed_out(AccountPage::Register);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-register"));
            let ui = signed_out(AccountPage::CheckEmail);
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-check-email"));
            let ui = signed_out(AccountPage::SetPassword);
            let account = ui.global::<Account>();
            account.set_password("short".into());
            account.set_error(account.invoke_message("short".into()));
            render(&window, &ui, 1024.0, |_| 684.0, &name("vault-set-password"));
            // A short window scrolls rather than clipping the form.
            render(
                &window,
                &ui,
                1024.0,
                |_| 520.0,
                &name("vault-set-password-short"),
            );
            let ui = quick(QuickState::Message);
            ui.global::<Vault>().set_notice(QuickNotice::LoggedOut);
            render(&window, &ui, 624.0, |_| 504.0, &name("quick-signed-out"));

            let ui = SettingsWindow::new().unwrap();
            setup(&ui, lang, dark);
            ui.set_idle(2);
            ui.set_clear_clipboard(true);
            ui.set_clear_after(2);
            ui.set_ask_read(true);
            ui.set_remember(1);
            ui.set_lock_on_screen_lock(true);
            ui.set_lock_on_sleep(true);
            ui.set_allow_without_window(true);
            ui.set_browser(true);
            ui.set_browser_others("Google Chrome".into());
            render(&window, &ui, 664.0, |_| 724.0, &name("settings"));
            // Scrolled to the end, with the wheel.
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(300.0, 400.0),
                delta_x: 0.0,
                delta_y: -3000.0,
            });
            render(&window, &ui, 664.0, |_| 724.0, &name("settings-end"));

            let ui = ApproveWindow::new().unwrap();
            setup(&ui, lang, dark);
            show_access_approve(&ui, &read, Some(&terminal), Scope::Terminal);
            render(
                &window,
                &ui,
                WIDTH,
                |ui| ui.get_natural_height(),
                &name("approve-secret"),
            );

            let ui = ApproveWindow::new().unwrap();
            setup(&ui, lang, dark);
            show_access_approve(&ui, &import, Some(&terminal), Scope::Request);
            render(
                &window,
                &ui,
                WIDTH,
                |ui| ui.get_natural_height(),
                &name("approve-import"),
            );

            let ui = ApproveWindow::new().unwrap();
            setup(&ui, lang, dark);
            show_access_approve(&ui, &browser, Some(&firefox), Scope::Request);
            render(
                &window,
                &ui,
                WIDTH,
                |ui| ui.get_natural_height(),
                &name("approve-browser"),
            );
        }
    }
}

fn quick_item(name: &str, user: &str, folder: &str) -> QuickItem {
    QuickItem {
        kind: "login".into(),
        favorite: false,
        name: name.into(),
        subtitle: user.into(),
        folder: folder.into(),
        initial: name[..1].into(),
        has_username: true,
        has_password: true,
        has_totp: name == "GitHub",
        has_website: true,
        trashed: false,
    }
}

fn setup<C>(ui: &C, lang: &str, dark: bool)
where
    C: ComponentHandle,
    for<'a> Theme<'a>: Global<'a, C>,
{
    slint::select_bundled_translation(lang).unwrap();
    ui.global::<Theme>().set_mode(if dark { 2 } else { 1 });
}

/// `height` is read once the window has its width, so wrapped text is measured.
fn render<C: ComponentHandle>(
    window: &MinimalSoftwareWindow,
    ui: &C,
    width: f32,
    height: impl Fn(&C) -> f32,
    path: &Path,
) {
    ui.show().unwrap();
    window.set_size(PhysicalSize::new(
        (width * SCALE) as u32,
        (1000.0 * SCALE) as u32,
    ));
    slint::platform::update_timers_and_animations();
    let size = PhysicalSize::new((width * SCALE) as u32, (height(ui) * SCALE) as u32);
    window.set_size(size);
    slint::platform::update_timers_and_animations();
    let mut pixels = vec![PremultipliedRgbaColor::default(); (size.width * size.height) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, size.width as usize);
    });
    ui.hide().unwrap();

    let file = BufWriter::new(File::create(path).unwrap());
    let mut encoder = png::Encoder::new(file, size.width, size.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    let bytes: Vec<u8> = pixels
        .iter()
        .flat_map(|p| [p.red, p.green, p.blue, p.alpha])
        .collect();
    writer.write_image_data(&bytes).unwrap();
}
