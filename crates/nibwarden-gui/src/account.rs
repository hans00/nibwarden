//! Signing in and creating an account in the main window (phase2.md §5.7). Shows the
//! `Account` global (account.slint) while no account is logged in.
//!
//! Like `transfer`, a worker thread talks to the daemon over a connection of its own. A
//! two-step login has to continue on the connection it started on: the daemon keeps the
//! credentials there until the code arrives.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use nibwarden_ipc::{
    Client, ClientError, ErrorCode, RegisterStartResult, Request, StatusResult, TwoFactorProvider,
};
use slint::{ComponentHandle, Global};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::{Account, AccountPage, vault_ui};

/// What Bitwarden requires of a new master password.
const MIN_PASSWORD: usize = 12;

enum Command {
    SignIn {
        server: String,
        email: String,
        password: Zeroizing<String>,
    },
    SendCode,
    TwoStep {
        provider: TwoFactorProvider,
        token: Zeroizing<String>,
    },
    Register {
        server: String,
        email: String,
        name: String,
    },
    /// Create the account, then sign in with it.
    Create {
        server: String,
        email: String,
        token: String,
        password: Zeroizing<String>,
        hint: Option<String>,
    },
}

/// Runs `Account` code on the UI thread, from any thread.
type Post = Arc<dyn Fn(Box<dyn FnOnce(&Account<'_>) + Send>) + Send + Sync>;

thread_local! {
    /// The token that lets the new account be created, from the server or the email.
    static TOKEN: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Connects the window's `Account`; `refresh` makes the window show the vault once signed
/// in.
pub fn attach<W>(ui: &W, refresh: impl Fn() + Send + Sync + 'static)
where
    W: ComponentHandle + 'static,
    for<'a> Account<'a>: Global<'a, W>,
{
    let (tx, rx) = mpsc::unbounded_channel();
    let post: Post = {
        let weak = ui.as_weak();
        Arc::new(move |f| {
            let _ = weak.upgrade_in_event_loop(move |ui| f(&ui.global::<Account>()));
        })
    };
    let refresh: Arc<dyn Fn() + Send + Sync> = Arc::new(refresh);
    std::thread::spawn(move || worker(rx, post, refresh));

    let global = ui.global::<Account>();
    let send = Rc::new(move |command| {
        let _ = tx.send(command);
    });

    // Runs `form` on a click: what it returns goes to the worker, or the key of what is
    // missing is shown.
    let submit = {
        let weak = ui.as_weak();
        move |form: fn(&Account<'_>) -> Result<Command, &'static str>| {
            let weak = weak.clone();
            let send = send.clone();
            move || {
                let Some(ui) = weak.upgrade() else { return };
                let a = ui.global::<Account>();
                if a.get_busy() {
                    return;
                }
                match form(&a) {
                    Ok(command) => {
                        a.set_error("".into());
                        a.set_busy(true);
                        send(command);
                    }
                    Err(key) => a.set_error(a.invoke_message(key.into())),
                }
            }
        }
    };
    global.on_sign_in(submit(sign_in_form));
    global.on_submit_code(submit(two_step_form));
    global.on_send_code(submit(|a| {
        a.set_code_sent(true);
        Ok(Command::SendCode)
    }));
    global.on_start_register(submit(register_form));
    global.on_create_account(submit(create_form));

    global.on_clean_code(|text| clean_code(&text).into());
    global.on_digit(|code, index| {
        usize::try_from(index)
            .ok()
            .and_then(|i| code.chars().nth(i))
            .map(String::from)
            .unwrap_or_default()
            .into()
    });

    let weak = ui.as_weak();
    global.on_use_link(move || {
        let Some(ui) = weak.upgrade() else { return };
        let a = ui.global::<Account>();
        let link = a.get_link();
        if link.trim().is_empty() {
            return a.set_error(a.invoke_message("link".into()));
        }
        let Some((token, email)) = parse_link(&link) else {
            return a.set_error(a.invoke_message("bad-link".into()));
        };
        if let Some(email) = email {
            a.set_email(email.into());
        }
        TOKEN.with(|t| *t.borrow_mut() = Some(token));
        go(&a, AccountPage::SetPassword);
    });

    let weak = ui.as_weak();
    global.on_go(move |page| {
        if let Some(ui) = weak.upgrade() {
            go(&ui.global::<Account>(), page);
        }
    });
}

/// Shows `page`, forgetting what was typed there that should not linger.
fn go(a: &Account<'_>, page: AccountPage) {
    a.set_password("".into());
    a.set_password_again("".into());
    a.set_code("".into());
    a.set_error("".into());
    a.set_busy(false);
    if page != AccountPage::SetPassword {
        a.set_hint("".into());
    }
    if page == AccountPage::Register {
        a.set_link("".into());
        TOKEN.with(|t| *t.borrow_mut() = None);
    }
    a.set_page(page);
}

/// The server as the daemon takes it.
fn server(a: &Account<'_>) -> Result<String, &'static str> {
    match a.get_server_kind() {
        0 => Ok("bitwarden.com".into()),
        1 => Ok("bitwarden.eu".into()),
        _ => {
            let url = a.get_server_url().trim().trim_end_matches('/').to_owned();
            match url.as_str() {
                "" => Err("server-url"),
                _ if url.contains("://") => Ok(url),
                _ => Ok(format!("https://{url}")),
            }
        }
    }
}

fn email(a: &Account<'_>) -> Result<String, &'static str> {
    let email = normalize_email(&a.get_email());
    if email.is_empty() || !email.contains('@') {
        return Err("email");
    }
    Ok(email)
}

/// As the daemon spells addresses (`nibwarden_core::normalize_email`), so the page shows
/// what is used.
fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn sign_in_form(a: &Account<'_>) -> Result<Command, &'static str> {
    let server = server(a)?;
    let email = email(a)?;
    let password = Zeroizing::new(a.get_password().to_string());
    if password.is_empty() {
        return Err("password");
    }
    Ok(Command::SignIn {
        server,
        email,
        password,
    })
}

/// Bitwarden's two-step and new device codes are six digits.
const CODE_LENGTH: usize = 6;

/// Only the digits of what was typed or pasted (`123 456`, `123-456`), and no more than a
/// code has.
fn clean_code(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_digit)
        .take(CODE_LENGTH)
        .collect()
}

fn two_step_form(a: &Account<'_>) -> Result<Command, &'static str> {
    let token = clean_code(&a.get_code());
    if token.len() < CODE_LENGTH {
        return Err("code");
    }
    let provider = if a.get_new_device() {
        TwoFactorProvider::NewDevice
    } else if a.get_use_email() {
        TwoFactorProvider::Email
    } else {
        TwoFactorProvider::Authenticator
    };
    Ok(Command::TwoStep {
        provider,
        token: Zeroizing::new(token),
    })
}

fn register_form(a: &Account<'_>) -> Result<Command, &'static str> {
    Ok(Command::Register {
        server: server(a)?,
        email: email(a)?,
        name: a.get_name().trim().to_owned(),
    })
}

fn create_form(a: &Account<'_>) -> Result<Command, &'static str> {
    let password = Zeroizing::new(a.get_password().to_string());
    if password.chars().count() < MIN_PASSWORD {
        return Err("short");
    }
    if *password != *a.get_password_again() {
        return Err("mismatch");
    }
    let hint = a.get_hint().trim().to_owned();
    if hint == *password {
        return Err("hint-is-password");
    }
    let token = TOKEN.with(|t| t.borrow().clone()).ok_or("link")?;
    Ok(Command::Create {
        server: server(a)?,
        email: email(a)?,
        token,
        password,
        hint: (!hint.is_empty()).then_some(hint),
    })
}

/// The token (and address) in the link from the verification email, wherever the server
/// put them: `…/#/finish-signup/?email=…&token=…` (Vaultwarden),
/// `…/redirect-connector.html#finish-signup?token=…&email=…` (Bitwarden). A bare token
/// pasted on its own is taken as it is.
fn parse_link(link: &str) -> Option<(String, Option<String>)> {
    let link = link.trim();
    if !link.contains("://") {
        let bare = !link.is_empty() && !link.contains(char::is_whitespace) && link.len() >= 16;
        return bare.then(|| (link.to_owned(), None));
    }
    let mut token = None;
    let mut email = None;
    for pair in link.split(['?', '&', '#']) {
        match pair.split_once('=') {
            Some(("token", value)) => token = Some(percent_decode(value)),
            Some(("email", value)) => email = Some(percent_decode(value)),
            _ => {}
        }
    }
    let token = token.filter(|t| !t.is_empty())?;
    Some((token, email.filter(|e| e.contains('@'))))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
                fail_key(&post, "no-daemon");
                continue;
            };
            if let Err(e) = run(c, &post, &*refresh, command).await {
                if !matches!(e, ClientError::Remote(_)) {
                    client = None;
                }
                fail(&post, &e);
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
        Command::SignIn {
            server,
            email,
            password,
        } => sign_in(client, post, refresh, server, email, password).await,
        Command::SendCode => {
            client.call::<()>(Request::SendEmailCode).await?;
            post(Box::new(|a| a.set_busy(false)));
            Ok(())
        }
        Command::TwoStep { provider, token } => {
            client
                .call::<StatusResult>(Request::Login2fa { provider, token })
                .await?;
            signed_in(post, refresh);
            Ok(())
        }
        Command::Register {
            server,
            email,
            name,
        } => {
            let started: RegisterStartResult = client
                .call(Request::RegisterStart {
                    server,
                    email: email.clone(),
                    name,
                })
                .await?;
            post(Box::new(move |a| {
                // As the account will be spelled from now on.
                a.set_email(email.into());
                let page = match started.token {
                    Some(token) => {
                        TOKEN.with(|t| *t.borrow_mut() = Some(token));
                        AccountPage::SetPassword
                    }
                    None => AccountPage::CheckEmail,
                };
                go(a, page);
            }));
            Ok(())
        }
        Command::Create {
            server,
            email,
            token,
            password,
            hint,
        } => {
            client
                .call::<()>(Request::RegisterFinish {
                    server: server.clone(),
                    email: email.clone(),
                    token,
                    password: password.clone(),
                    hint,
                })
                .await?;
            post(Box::new(|_| TOKEN.with(|t| *t.borrow_mut() = None)));
            sign_in(client, post, refresh, server, email, password).await
        }
    }
}

async fn sign_in(
    client: &mut Client,
    post: &Post,
    refresh: &(dyn Fn() + Send + Sync),
    server: String,
    email: String,
    password: Zeroizing<String>,
) -> Result<(), ClientError> {
    let result = client
        .call::<StatusResult>(Request::Login {
            server,
            email,
            password,
        })
        .await;
    match result {
        Ok(_) => signed_in(post, refresh),
        Err(ClientError::Remote(e)) if e.code == ErrorCode::AlreadyLoggedIn => {
            signed_in(post, refresh)
        }
        Err(ClientError::Remote(e))
            if e.code == ErrorCode::TwoFactorRequired
                && e.providers.contains(&TwoFactorProvider::NewDevice) =>
        {
            // The server has emailed the code already.
            post(Box::new(|a| {
                go(a, AccountPage::TwoStep);
                a.set_new_device(true);
                a.set_can_authenticator(false);
                a.set_can_email(false);
                a.set_use_email(true);
                a.set_code_sent(true);
            }));
        }
        Err(ClientError::Remote(e)) if e.code == ErrorCode::TwoFactorRequired => {
            let authenticator = e.providers.contains(&TwoFactorProvider::Authenticator);
            let email = e.providers.contains(&TwoFactorProvider::Email);
            // Authenticator first: it needs no round trip through the mailbox.
            let use_email = !authenticator && email;
            post(Box::new(move |a| {
                go(a, AccountPage::TwoStep);
                a.set_new_device(false);
                a.set_can_authenticator(authenticator);
                a.set_can_email(email);
                a.set_use_email(use_email);
                a.set_code_sent(use_email);
                a.set_busy(use_email);
            }));
            if use_email {
                client.call::<()>(Request::SendEmailCode).await?;
                post(Box::new(|a| a.set_busy(false)));
            }
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Back to a clean sign-in page for next time, and the vault in the window.
fn signed_in(post: &Post, refresh: &(dyn Fn() + Send + Sync)) {
    post(Box::new(|a| {
        go(a, AccountPage::SignIn);
        a.set_name("".into());
        a.set_link("".into());
    }));
    refresh();
}

fn fail(post: &Post, e: &ClientError) {
    let message = match e {
        // The server's own words, without the daemon's preface.
        ClientError::Remote(body) => body
            .message
            .strip_prefix("server refused: ")
            .unwrap_or(&body.message)
            .to_owned(),
        _ => e.to_string(),
    };
    post(Box::new(move |a| {
        a.set_busy(false);
        a.set_error(message.into());
    }));
}

fn fail_key(post: &Post, key: &'static str) {
    post(Box::new(move |a| {
        a.set_busy(false);
        a.set_error(a.invoke_message(key.into()));
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_codes() {
        assert_eq!(clean_code("123 456"), "123456");
        assert_eq!(clean_code("12-34-56-78"), "123456");
        assert_eq!(clean_code("abc"), "");
    }

    #[test]
    fn reads_verification_links() {
        let vaultwarden = "https://vault.example.com/#/finish-signup/?email=alice%40example.com&token=eyJ0eXAi.abc_def-1";
        assert_eq!(
            parse_link(vaultwarden),
            Some((
                "eyJ0eXAi.abc_def-1".into(),
                Some("alice@example.com".into())
            ))
        );
        let bitwarden = "https://vault.bitwarden.com/redirect-connector.html#finish-signup?token=CfDJ8%2BAb%3D%3D&email=bob@example.com&fromEmail=true";
        assert_eq!(
            parse_link(bitwarden),
            Some(("CfDJ8+Ab==".into(), Some("bob@example.com".into())))
        );
        assert_eq!(
            parse_link("  eyJ0eXAiOiJKV1QiLCJhbGciOi  "),
            Some(("eyJ0eXAiOiJKV1QiLCJhbGciOi".into(), None))
        );
        assert_eq!(parse_link("https://vault.example.com/#/login"), None);
        assert_eq!(parse_link("hello there"), None);
    }
}
