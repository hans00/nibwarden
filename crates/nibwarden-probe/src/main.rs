//! Test tool for `scripts/e2e.sh`, not installed: through
//! nibwarden-core only, it registers accounts on test servers, logs in as a second device,
//! creates test items and Sends, and plays the Bitwarden browser extension.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use nibwarden_core::{Account, ItemQuery, NewLogin, Server, Store, TwoFactor, TwoFactorProvider};

#[derive(Parser)]
struct Cli {
    /// Data directory (SDK database, device id, unlock data).
    #[arg(long, env = "NIBWARDEN_DATA_DIR")]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an account on a self-hosted test server (Vaultwarden with signups enabled).
    Register {
        #[arg(long)]
        server: String,
        #[arg(long)]
        email: String,
    },
    /// Log in, then do a full sync.
    Login {
        /// `bitwarden.com`, `bitwarden.eu`, or a self-hosted base URL.
        #[arg(long)]
        server: String,
        #[arg(long)]
        email: String,
        /// Authenticator (TOTP) code.
        #[arg(long, conflicts_with = "email_code")]
        totp: Option<String>,
        /// Code from a two-factor email.
        #[arg(long)]
        email_code: Option<String>,
    },
    /// Restore the session from disk (locked), unlock, sync, and list items.
    Unlock,
    /// Show whether a session exists, without unlocking.
    Status,
    /// Open a Send link the way a recipient's browser does: print its name and text as
    /// JSON, and save its file to `--out`.
    OpenSend {
        link: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write a KeePass database with a "KeePass" group holding one login entry, to test
    /// importing (test data, so taken from argv).
    MakeKdbx {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        password: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        secret: String,
    },
    /// Unlock, create a login item, then read its password back by id.
    AddLogin {
        #[arg(long)]
        name: String,
        #[arg(long)]
        username: String,
        /// Password stored in the new item (test data, so taken from argv).
        #[arg(long)]
        secret: String,
        /// TOTP secret (base32 or otpauth:// URI).
        #[arg(long)]
        totp: Option<String>,
        #[arg(long)]
        uri: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        /// Hidden custom field, `NAME=VALUE`; repeatable.
        #[arg(long = "field", value_parser = parse_field)]
        fields: Vec<(String, String)>,
        /// Id of the folder to put the item in (see `add-folder`).
        #[arg(long)]
        folder_id: Option<String>,
        /// Ask for the master password again before use.
        #[arg(long)]
        reprompt: bool,
    },
    /// Act as the Bitwarden browser extension against `nibwarden-browser-host`: set up the
    /// channel, ask for the status, then ask to be unlocked. Prints one line per step.
    BrowserExtension {
        #[arg(long)]
        host: PathBuf,
        /// Claim another account's user id.
        #[arg(long)]
        user_id: Option<String>,
    },
    /// Unlock, set a PIN, lock, and unlock with it (wrong, then right); then the same with
    /// the kept key (system authentication). Prints one line per step.
    QuickUnlock,
    /// Unlock and create a folder; prints its id.
    AddFolder {
        #[arg(long)]
        name: String,
    },
    /// Unlock and store an unencrypted private key file as an SSH key item; prints its id.
    AddSshKey {
        #[arg(long)]
        name: String,
        #[arg(long)]
        private_key_file: PathBuf,
    },
}

fn parse_field(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(n, v)| (n.to_owned(), v.to_owned()))
        .ok_or_else(|| "expected NAME=VALUE".to_owned())
}

/// Reads the master password from `NIBWARDEN_PASSWORD` (for scripted tests) or the TTY.
fn read_password() -> Result<String> {
    if let Ok(pw) = std::env::var("NIBWARDEN_PASSWORD") {
        return Ok(pw);
    }
    Ok(rpassword::prompt_password("Master password: ")?)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let store = Store::open(&cli.data_dir)?;

    match cli.command {
        Command::Register { server, email } => register(&Server::parse(&server), &email).await,
        Command::Login {
            server,
            email,
            totp,
            email_code,
        } => {
            let two_factor = match (totp, email_code) {
                (Some(token), _) => Some(TwoFactor {
                    provider: TwoFactorProvider::Authenticator,
                    token,
                }),
                (_, Some(token)) => Some(TwoFactor {
                    provider: TwoFactorProvider::Email,
                    token,
                }),
                _ => None,
            };
            let password = read_password()?;
            let account = Account::login_password(
                store,
                &Server::parse(&server),
                &email,
                &password,
                two_factor,
            )
            .await?;
            println!("logged in as {}", account.email());
            print_items(&account).await
        }
        Command::Unlock => {
            let account = Account::load(store).await?.context("not logged in")?;
            println!(
                "restored session for {} (unlocked: {})",
                account.email(),
                account.is_unlocked()
            );
            account.unlock_with_password(&read_password()?).await?;
            println!("unlocked: {}", account.is_unlocked());
            let synced = account.sync(false).await?;
            println!("sync ran: {synced}");
            print_items(&account).await?;
            account.lock().await;
            println!("locked again: {}", !account.is_unlocked());
            match account.list_items(false).await {
                Err(nibwarden_core::Error::Locked) => println!("list after lock: refused (ok)"),
                other => bail!("expected Locked after lock, got {other:?}"),
            }
            Ok(())
        }
        Command::AddLogin {
            name,
            username,
            secret,
            totp,
            uri,
            notes,
            fields,
            folder_id,
            reprompt,
        } => {
            let account = Account::load(store).await?.context("not logged in")?;
            account.unlock_with_password(&read_password()?).await?;
            let fields: Vec<(&str, &str)> = fields
                .iter()
                .map(|(n, v)| (n.as_str(), v.as_str()))
                .collect();
            let id = account
                .create_login(&NewLogin {
                    name: &name,
                    username: Some(&username),
                    password: Some(&secret),
                    totp: totp.as_deref(),
                    uri: uri.as_deref(),
                    notes: notes.as_deref(),
                    fields: &fields,
                    folder: folder_id.as_deref(),
                    reprompt,
                })
                .await?;
            println!("created {id}");
            let query = ItemQuery {
                folder: None,
                item: &id,
            };
            let read_back = account.read_field(query, "password").await?;
            if *read_back.value != secret {
                bail!("read-back mismatch");
            }
            println!("password read back and matches");
            Ok(())
        }
        Command::BrowserExtension { host, user_id } => {
            let account = Account::load(store).await?.context("not logged in")?;
            account.unlock_with_password(&read_password()?).await?;
            let expected_key = account.user_key().await?;
            let user_id = user_id.unwrap_or_else(|| account.user_id().to_owned());
            browser_extension(&host, &user_id, &expected_key).await
        }
        Command::QuickUnlock => {
            let account = Account::load(store.clone())
                .await?
                .context("not logged in")?;
            account.unlock_with_password(&read_password()?).await?;
            account.set_pin(Some("4711"))?;
            account.lock().await;
            println!(
                "wrong pin: {:?}",
                account
                    .unlock_with_pin("0000")
                    .await
                    .err()
                    .map(|e| e.to_string())
            );
            account.unlock_with_pin("4711").await?;
            println!("right pin: {}", account.is_unlocked());
            account.keep_key(true).await?;
            account.lock().await;
            account.unlock_with_kept_key().await?;
            println!("kept key: {}", account.is_unlocked());
            // A new process: the PIN works again only after the master password.
            let again = Account::load(store).await?.context("not logged in")?;
            println!("pin before password: {}", again.pin_ready());
            again.unlock_with_password(&read_password()?).await?;
            again.lock().await;
            again.unlock_with_pin("4711").await?;
            println!("pin after password: {}", again.is_unlocked());
            again.set_pin(None)?;
            println!("pin removed: {}", !again.has_pin());
            Ok(())
        }
        Command::AddFolder { name } => {
            let account = Account::load(store).await?.context("not logged in")?;
            account.unlock_with_password(&read_password()?).await?;
            println!("{}", account.create_folder(&name).await?);
            Ok(())
        }
        Command::AddSshKey {
            name,
            private_key_file,
        } => {
            let pem = std::fs::read_to_string(&private_key_file)
                .with_context(|| format!("reading {}", private_key_file.display()))?;
            let account = Account::load(store).await?.context("not logged in")?;
            account.unlock_with_password(&read_password()?).await?;
            println!("{}", account.import_ssh_key(&name, &pem).await?);
            Ok(())
        }
        Command::OpenSend { link, out } => open_send(&link, out.as_deref()).await,
        Command::MakeKdbx {
            out,
            password,
            title,
            secret,
        } => {
            use keepass::{Database, DatabaseKey, db::fields};
            let mut db = Database::new();
            {
                let mut root = db.root_mut();
                let mut group = root.add_group();
                group.name = "KeePass".into();
                let mut entry = group.add_entry();
                entry.set_unprotected(fields::TITLE, title);
                entry.set_unprotected(fields::USERNAME, "keeper");
                entry.set_protected(fields::PASSWORD, secret);
                entry.set_unprotected(fields::URL, "https://keepass.test");
            }
            let mut bytes = Vec::new();
            db.save(&mut bytes, DatabaseKey::new().with_password(&password))?;
            std::fs::write(out, bytes)?;
            Ok(())
        }
        Command::Status => {
            match Account::load(store).await? {
                Some(a) => println!("logged in as {} (unlocked: {})", a.email(), a.is_unlocked()),
                None => println!("logged out"),
            }
            Ok(())
        }
    }
}

async fn open_send(link: &str, out: Option<&std::path::Path>) -> Result<()> {
    let received = nibwarden_core::receive_send(link).await?;
    println!(
        "{}",
        serde_json::json!({ "name": received.name, "text": received.text,
            "file_name": received.file_name })
    );
    if let (Some(url), Some(out)) = (&received.file_url, out) {
        let (_, key) = nibwarden_core::parse_send_link(link).context("not a Send link")?;
        // The server builds the URL from its configured domain; keep the path and use
        // the address the link came from.
        let base = link.split_once("/#/send/").context("not a Send link")?.0;
        let path = url.find("/api/").map_or(url.as_str(), |i| &url[i..]);
        let data = reqwest::get(format!("{base}{path}"))
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        std::fs::write(out, nibwarden_core::decrypt_send_file(key, &data)?)?;
    }
    Ok(())
}

async fn print_items(account: &Account) -> Result<()> {
    let items = account.list_items(false).await?;
    println!("{} item(s):", items.len());
    for item in items {
        println!(
            "  [{:<8}] {}  {}  ({})",
            item.kind, item.name, item.subtitle, item.id
        );
    }
    Ok(())
}

async fn register(server: &Server, email: &str) -> Result<()> {
    let password = read_password()?;
    let reg = nibwarden_core::make_registration(email, &password)?;
    let http = reqwest::Client::new();

    // Two-step sign-up (Vaultwarden >= 1.34). With mail disabled, the verification token
    // comes back in the response body instead of an email.
    let resp = http
        .post(format!(
            "{}/accounts/register/send-verification-email",
            server.identity_url
        ))
        .json(
            &serde_json::json!({ "email": email, "name": email, "receiveMarketingEmails": false }),
        )
        .send()
        .await?;
    let status = resp.status();
    let token = resp.text().await?;
    if !status.is_success() || token.is_empty() {
        bail!(
            "verification token request failed ({status}); is SIGNUPS_VERIFY off and mail unset?"
        );
    }

    let body = serde_json::json!({
        "email": email,
        "emailVerificationToken": token.trim_matches('"'),
        "masterPasswordHash": reg.master_password_hash,
        "masterPasswordHint": null,
        "userSymmetricKey": reg.encrypted_user_key,
        "userAsymmetricKeys": {
            "publicKey": reg.public_key,
            "encryptedPrivateKey": reg.encrypted_private_key,
        },
        "kdf": 0,
        "kdfIterations": reg.kdf_iterations,
    });
    let resp = http
        .post(format!("{}/accounts/register/finish", server.identity_url))
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        bail!(
            "register failed: {status} {}",
            resp.text().await.unwrap_or_default()
        );
    }
    println!("registered {email}");
    Ok(())
}

/// The extension's half of the desktop integration, as `nativeMessaging.background.ts`
/// does it.
async fn browser_extension(
    host: &std::path::Path,
    user_id: &str,
    expected_key: &str,
) -> Result<()> {
    use nibwarden_core::ExtensionKeys;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut child = tokio::process::Command::new(host)
        .arg("chrome-extension://nngceckbapebfimnlniiiahkandclblb/")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", host.display()))?;
    let mut to_host = child.stdin.take().context("no stdin")?;
    let mut from_host = child.stdout.take().context("no stdout")?;
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default()
    };
    let mut read = async || -> Result<Value> {
        let mut head = [0u8; 4];
        from_host.read_exact(&mut head).await?;
        let mut body = vec![0u8; u32::from_ne_bytes(head) as usize];
        from_host.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    };
    let first = read().await?;
    println!("{}", first["command"].as_str().unwrap_or("?"));
    if first["command"] != "connected" {
        return Ok(());
    }

    let mut send = async |message: Value| -> Result<()> {
        let body = message.to_string().into_bytes();
        to_host
            .write_all(&(body.len() as u32).to_ne_bytes())
            .await?;
        to_host.write_all(&body).await?;
        to_host.flush().await?;
        Ok(())
    };
    let (keys, public_key) = ExtensionKeys::new()?;
    send(json!({ "appId": "e2e", "message": {
        "command": "setupEncryption", "publicKey": public_key, "userId": user_id,
        "messageId": 0, "timestamp": now(),
    }}))
    .await?;
    let setup = read().await?;
    if setup["command"] != "setupEncryption" {
        println!("setup: {}", setup["command"].as_str().unwrap_or("?"));
        return Ok(());
    }
    let channel = keys.accept(setup["sharedSecret"].as_str().context("no sharedSecret")?)?;
    println!("setup: ok");

    for (id, command) in [
        (1, "getBiometricsStatusForUser"),
        (2, "unlockWithBiometricsForUser"),
    ] {
        let message = json!({
            "command": command, "userId": user_id, "messageId": id, "timestamp": now(),
        });
        send(json!({ "appId": "e2e", "message": channel.seal(&message.to_string())? })).await?;
        let reply = read().await?;
        let reply: Value = serde_json::from_str(&channel.open(&reply["message"])?)?;
        anyhow::ensure!(
            reply["messageId"] == id,
            "answer to another message: {reply}"
        );
        match command {
            "getBiometricsStatusForUser" => println!("status: {}", reply["response"]),
            _ => {
                let key = reply["userKeyB64"].as_str();
                let matches = key == Some(expected_key);
                println!(
                    "unlock: {}{}",
                    reply["response"],
                    if matches { " key-matches" } else { "" }
                );
            }
        }
    }
    Ok(())
}
