//! `nibwarden`: talks to `nibwardend` over the control socket (phase1.md §7). Secrets
//! are only ever read from the terminal (or stdin with `--password-stdin`), never argv.

use std::{
    collections::HashMap,
    ffi::OsString,
    io::{BufRead as _, IsTerminal as _, Write as _},
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use nibwarden_cli::{call_vault, connect};
use nibwarden_ipc::{
    Client, ClientError, ErrorCode, ItemInfo, ItemListResult, LockState, PassphraseParams,
    PasswordParams, Reference, Request, SshKeyEntry, SshListResult, StatusResult, SyncResult,
    TotpResult, TwoFactorProvider, ValueResult,
};
use nibwarden_platform::{process, terminal};
use zeroize::Zeroizing;

mod send;
mod transfer;

#[derive(Parser)]
#[command(name = "nibwarden", version, about)]
struct Cli {
    /// Control socket of nibwardend [default: $XDG_RUNTIME_DIR/nibwarden/daemon.sock].
    #[arg(long, global = true, env = "NIBWARDEN_SOCKET")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show login and lock state.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Log in to a Bitwarden-compatible server.
    Login {
        /// `bitwarden.com`, `bitwarden.eu`, or the base URL of a self-hosted server.
        #[arg(long, default_value = "bitwarden.com")]
        server: String,
        #[arg(long)]
        email: String,
        /// Read the master password from the first line of stdin instead of the terminal.
        #[arg(long)]
        password_stdin: bool,
        /// Two-step method to use when the account offers several.
        #[arg(long, value_enum)]
        method: Option<Method>,
    },
    /// Log out and delete the local session and vault cache.
    Logout,
    /// Unlock with the master password.
    ///
    /// Asks in nibwarden's unlock window when one can be shown (a desktop session, not
    /// over SSH), otherwise on the terminal.
    Unlock {
        /// Read the master password from the first line of stdin.
        #[arg(long)]
        password_stdin: bool,
        /// Always ask in the unlock window.
        #[arg(long, conflicts_with_all = ["password_stdin", "terminal"])]
        gui: bool,
        /// Always ask on the terminal.
        #[arg(long, conflicts_with = "password_stdin")]
        terminal: bool,
    },
    /// Lock now.
    Lock,
    /// Sync with the server.
    Sync {
        /// Fetch everything even if the server reports no changes.
        #[arg(long)]
        force: bool,
    },
    /// List items (names, kinds, folders, URIs; never secrets).
    List {
        /// Case-insensitive match on name, subtitle or URI.
        search: Option<String>,
        #[arg(long)]
        folder: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print one field of an item.
    Get {
        /// Item name or id, or a full `nw://` reference.
        item: String,
        /// `password`, `username`, `totp`, `notes`, `uri`, card/SSH fields, or a custom field.
        #[arg(long, short, default_value = "password")]
        field: String,
        #[arg(long)]
        folder: Option<String>,
        /// Do not print a trailing newline.
        #[arg(long, short)]
        no_newline: bool,
    },
    /// Print the value of a `nw://[folder/]item/field` reference (for scripts).
    Read {
        reference: String,
        /// Do not print a trailing newline.
        #[arg(long, short)]
        no_newline: bool,
    },
    /// Print the current TOTP code of an item.
    Totp {
        /// Item name or id.
        item: String,
        #[arg(long)]
        folder: Option<String>,
    },
    /// Run a command with `nw://` references in its environment replaced by their values.
    Run {
        /// `KEY=VALUE` file (values may be `nw://` references); repeatable, later wins.
        #[arg(long)]
        env_file: Vec<PathBuf>,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Generate a password or passphrase.
    Generate {
        /// Password length.
        #[arg(long, short, default_value_t = 20)]
        length: u8,
        /// Include special characters (! @ # $ % ^ & *).
        #[arg(long)]
        special: bool,
        #[arg(long)]
        no_uppercase: bool,
        #[arg(long)]
        no_lowercase: bool,
        #[arg(long)]
        no_numbers: bool,
        /// Avoid I, O, l, 0 and 1.
        #[arg(long)]
        avoid_ambiguous: bool,
        /// Generate a passphrase instead.
        #[arg(long)]
        passphrase: bool,
        #[arg(long, default_value_t = 5)]
        words: u8,
        #[arg(long, default_value = "-")]
        separator: String,
        #[arg(long)]
        capitalize: bool,
        /// Append a digit to one word.
        #[arg(long)]
        include_number: bool,
    },
    /// SSH agent.
    Ssh {
        #[command(subcommand)]
        command: SshCommand,
    },
    /// Let the Bitwarden browser extension unlock with nibwarden, in place of Bitwarden's
    /// desktop app (its "Unlock with biometrics").
    Browser {
        #[command(subcommand)]
        command: BrowserCommand,
    },
    /// Share text or a file through a link (Bitwarden Send).
    Send {
        #[command(subcommand)]
        command: send::SendCommand,
    },
    /// Export the personal vault to a file (asks for the master password again).
    Export {
        #[arg(long, value_enum, default_value = "encrypted-json")]
        format: transfer::Format,
        /// The file to write (created readable only by you).
        #[arg(long, short)]
        output: PathBuf,
        /// Replace the file if it exists.
        #[arg(long)]
        force: bool,
        /// Read the master password (and then the file password) from stdin lines.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Import a Bitwarden JSON or CSV export, or a KeePass database (.kdbx).
    Import {
        file: PathBuf,
        /// Read the file's password, if it has one, from the first line of stdin.
        #[arg(long)]
        password_stdin: bool,
    },
}

#[derive(Subcommand)]
enum SshCommand {
    /// List the SSH keys the agent offers.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Allow SSH signatures (logins, git) from this terminal session with the master
    /// password, for when no approval window can be shown (e.g. over SSH). Lasts until
    /// the vault locks.
    Authorize {
        /// Read the master password from the first line of stdin instead of the terminal.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Make ssh use nibwarden's agent (an IdentityAgent block in ~/.ssh/config).
    AgentSetup {
        /// Remove nibwarden's block instead.
        #[arg(long)]
        remove: bool,
    },
    /// Make git sign commits with a vault SSH key (gpg.format=ssh, commit.gpgsign=true).
    GitSetup {
        /// Key name or id; needed when the vault has more than one SSH key.
        #[arg(long)]
        key: Option<String>,
        /// Write the global git config instead of the current repository's.
        #[arg(long)]
        global: bool,
        /// Stop signing commits through nibwarden (global git config).
        #[arg(long, conflicts_with_all = ["key", "global"])]
        remove: bool,
    },
}

#[derive(Subcommand)]
enum BrowserCommand {
    /// Register nibwarden with the installed browsers.
    Enable {
        /// Also where Bitwarden's desktop app is registered.
        #[arg(long)]
        replace: bool,
    },
    /// Remove nibwarden's registration.
    Disable,
    /// Show what each installed browser starts for the extension.
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum Method {
    Authenticator,
    Email,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nibwarden: {e:#}");
            // Distinct codes for the states scripts most often need to react to.
            match e.downcast_ref::<ClientError>() {
                Some(ClientError::Remote(r)) if r.code == ErrorCode::Locked => ExitCode::from(3),
                Some(ClientError::Remote(r)) if r.code == ErrorCode::LoggedOut => ExitCode::from(4),
                _ => ExitCode::FAILURE,
            }
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    // Quick access talks to the daemon itself.
    if let Command::Browser { command } = &cli.command {
        return browser(command);
    }
    let mut client = connect(cli.socket.as_deref(), "nibwarden-cli").await?;
    match cli.command {
        Command::Status { json } => {
            let status: StatusResult = client.call(Request::Status).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_status(&status);
            }
        }
        Command::Login {
            server,
            email,
            password_stdin,
            method,
        } => {
            let password = read_secret("Master password: ", password_stdin)?;
            let first = client
                .call::<StatusResult>(Request::Login {
                    server,
                    email,
                    password,
                })
                .await;
            let status = match first {
                Err(ClientError::Remote(e)) if e.code == ErrorCode::TwoFactorRequired => {
                    two_factor(&mut client, &e.providers, method).await?
                }
                other => other?,
            };
            eprintln!("Logged in as {}.", status.email.as_deref().unwrap_or("?"));
        }
        Command::Logout => {
            client.call::<()>(Request::Logout).await?;
            eprintln!("Logged out.");
        }
        Command::Unlock {
            password_stdin,
            gui,
            terminal,
        } => {
            let status: StatusResult = client.call(Request::Status).await?;
            match status.state {
                LockState::LoggedOut => bail!("not logged in; run `nibwarden login` first"),
                LockState::Unlocked => eprintln!("Already unlocked."),
                LockState::Locked => {
                    let window = gui
                        || (!password_stdin
                            && !terminal
                            && client
                                .call::<nibwarden_ipc::CanPromptResult>(Request::CanPrompt)
                                .await?
                                .available);
                    if window {
                        client.call::<()>(Request::UnlockPrompt).await?;
                    } else {
                        if let Some(email) = &status.email {
                            eprintln!("Unlocking {email}");
                        }
                        let password = read_secret("Master password: ", password_stdin)?;
                        client.call::<()>(Request::Unlock { password }).await?;
                    }
                    eprintln!("Unlocked.");
                }
            }
        }
        Command::Lock => {
            client.call::<()>(Request::Lock).await?;
            eprintln!("Locked.");
        }
        Command::Sync { force } => {
            let result: SyncResult = client.call(Request::Sync { force }).await?;
            eprintln!(
                "{}",
                if result.ran {
                    "Synced."
                } else {
                    "Already up to date."
                }
            );
        }
        Command::List {
            search,
            folder,
            json,
        } => {
            let result: ItemListResult = call_vault(
                &mut client,
                Request::ItemList {
                    search,
                    folder,
                    trash: false,
                },
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result.items)?);
            } else {
                print_items(&result.items);
            }
        }
        Command::Get {
            item,
            field,
            folder,
            no_newline,
        } => {
            let request = if Reference::is_reference(&item) {
                reference_request(&item.parse()?)
            } else {
                Request::ItemGet {
                    folder,
                    item,
                    field,
                }
            };
            let result: ValueResult = call_vault(&mut client, request).await?;
            print_secret(&result.value, no_newline)?;
        }
        Command::Read {
            reference,
            no_newline,
        } => {
            let request = reference_request(&reference.parse()?);
            let result: ValueResult = call_vault(&mut client, request).await?;
            print_secret(&result.value, no_newline)?;
        }
        Command::Totp { item, folder } => {
            let result: TotpResult =
                call_vault(&mut client, Request::ItemTotp { folder, item }).await?;
            print_secret(&result.code, false)?;
            if std::io::stderr().is_terminal() {
                eprintln!("(valid for {}s)", result.remaining);
            }
        }
        Command::Run { env_file, command } => run_command(&mut client, &env_file, &command).await?,
        Command::Generate {
            length,
            special,
            no_uppercase,
            no_lowercase,
            no_numbers,
            avoid_ambiguous,
            passphrase,
            words,
            separator,
            capitalize,
            include_number,
        } => {
            let request = if passphrase {
                Request::GeneratePassphrase(PassphraseParams {
                    words,
                    separator,
                    capitalize,
                    include_number,
                })
            } else {
                Request::GeneratePassword(PasswordParams {
                    length,
                    uppercase: !no_uppercase,
                    lowercase: !no_lowercase,
                    numbers: !no_numbers,
                    special,
                    avoid_ambiguous,
                })
            };
            let result: ValueResult = client.call(request).await?;
            print_secret(&result.value, false)?;
        }
        Command::Browser { .. } => unreachable!("handled above"),
        Command::Ssh {
            command: SshCommand::Authorize { password_stdin },
        } => {
            let password = read_secret("Master password: ", password_stdin)?;
            client
                .call::<()>(Request::SshAuthorize { password })
                .await?;
            eprintln!(
                "SSH signatures from this terminal session are allowed until nibwarden locks."
            );
        }
        Command::Send { command } => send::run(&mut client, command).await?,
        Command::Export {
            format,
            output,
            force,
            password_stdin,
        } => transfer::export(&mut client, format, output, force, password_stdin).await?,
        Command::Import {
            file,
            password_stdin,
        } => transfer::import(&mut client, file, password_stdin).await?,
        Command::Ssh {
            command: SshCommand::GitSetup { remove: true, .. },
        } => {
            nibwarden_config::integration::git::unset_signing()?;
            eprintln!("git no longer signs commits through nibwarden.");
        }
        Command::Ssh {
            command: SshCommand::GitSetup { key, global, .. },
        } => git_setup(&mut client, key.as_deref(), global).await?,
        Command::Ssh {
            command: SshCommand::AgentSetup { remove },
        } => {
            use nibwarden_config::integration::ssh;
            if remove {
                ssh::set_agent(None)?;
                eprintln!(
                    "Removed nibwarden's agent from {}.",
                    ssh::config_path()?.display()
                );
            } else {
                let status: StatusResult = client.call(Request::Status).await?;
                let socket = status
                    .ssh_agent_socket
                    .context("the SSH agent is disabled in nibwardend")?;
                ssh::set_agent(Some(std::path::Path::new(&socket)))?;
                eprintln!("ssh uses {socket} ({}).", ssh::config_path()?.display());
                if let Some(other) = ssh::other_agent() {
                    eprintln!("Hosts with their own IdentityAgent keep it (e.g. {other}).");
                }
            }
        }
        Command::Ssh {
            command: SshCommand::List { json },
        } => {
            let result: SshListResult = call_vault(&mut client, Request::SshList).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result.keys)?);
                return Ok(());
            }
            if result.keys.is_empty() {
                eprintln!("No SSH keys in the vault.");
            }
            for key in &result.keys {
                println!("{}  {}  {}", key.name, key.fingerprint, key.id);
            }
            if std::io::stderr().is_terminal() {
                let status: StatusResult = client.call(Request::Status).await?;
                hint_agent_socket(status.ssh_agent_socket.as_deref());
            }
        }
    }
    Ok(())
}

/// Tells the user how to point ssh at the agent if their shell does not already.
fn hint_agent_socket(socket: Option<&str>) {
    let Some(socket) = socket else {
        eprintln!("The SSH agent is disabled in nibwardend.");
        return;
    };
    if std::env::var_os("SSH_AUTH_SOCK").is_some_and(|s| s == socket) {
        return;
    }
    eprintln!(
        "To use these keys, set SSH_AUTH_SOCK={socket} or add\n  \
         Host *\n    IdentityAgent {socket}\nto ~/.ssh/config."
    );
}

/// Writes the git settings of phase1.md §8.3 and trusts the key in the allowed signers
/// file, so `git log --show-signature` can verify the user's own commits.
/// Registers, unregisters or lists `nibwarden-browser-host` for the browsers.
fn browser(command: &BrowserCommand) -> Result<()> {
    use nibwarden_platform::native_messaging::{self, Registered};

    let host = std::env::current_exe()?.with_file_name(format!(
        "nibwarden-browser-host{}",
        std::env::consts::EXE_SUFFIX
    ));
    let browsers = match command {
        BrowserCommand::Enable { replace } => {
            if !host.exists() {
                bail!("{} is missing; reinstall nibwarden", host.display());
            }
            native_messaging::register(&host, *replace)?
        }
        BrowserCommand::Disable => {
            native_messaging::unregister(&host)?;
            native_messaging::browsers(&host)
        }
        BrowserCommand::Status => native_messaging::browsers(&host),
    };
    if browsers.is_empty() {
        eprintln!("No supported browser found (Firefox, Chrome, Chromium, Edge, Vivaldi, Brave).");
    }
    let mut others = false;
    for browser in &browsers {
        let state = match &browser.registered {
            Registered::Nibwarden => "nibwarden".to_owned(),
            Registered::Nothing => "-".to_owned(),
            Registered::Other(program) => {
                others = true;
                format!("another program: {program}")
            }
        };
        println!("{:<16} {state}", browser.name);
    }
    if others && matches!(command, BrowserCommand::Enable { replace: false }) {
        eprintln!(
            "Left as they are where another program (e.g. Bitwarden's desktop app) is \
             registered; `nibwarden browser enable --replace` registers nibwarden there too."
        );
    }
    if matches!(command, BrowserCommand::Enable { .. }) {
        eprintln!(
            "In the extension's settings, turn on \"Unlock with biometrics\"; it then asks \
             in nibwarden's window."
        );
    }
    Ok(())
}

async fn git_setup(client: &mut Client, wanted: Option<&str>, global: bool) -> Result<()> {
    use nibwarden_config::integration::git::{self, Scope};

    let keys: SshListResult = call_vault(client, Request::SshList).await?;
    let key = pick_key(&keys.keys, wanted)?;
    let scope = if global { Scope::Global } else { Scope::Local };
    // The helper sits next to this binary; fall back to $PATH if it was moved.
    let helper = std::env::current_exe()?.with_file_name(format!(
        "nibwarden-ssh-sign{}",
        std::env::consts::EXE_SUFFIX
    ));
    let helper = if helper.exists() {
        helper
    } else {
        PathBuf::from("nibwarden-ssh-sign")
    };
    let signing = git::set_signing(scope, &key.name, &key.public_key, &helper).map_err(|e| {
        if scope == Scope::Local && e.to_string() == "not in a git repository" {
            anyhow!("not in a git repository; use --global")
        } else {
            e.into()
        }
    })?;
    eprintln!(
        "Signing with \"{}\" ({}). Wrote to the {} git config:",
        key.name,
        key.fingerprint,
        if global { "global" } else { "local" }
    );
    for (name, value) in &signing.settings {
        eprintln!("  {name} = {value}");
    }
    match &signing.trusted {
        Some(trusted) => eprintln!(
            "{} {} in {}",
            if trusted.added {
                "Trusted"
            } else {
                "Already trusted:"
            },
            trusted.email,
            trusted.signers.display()
        ),
        None => eprintln!(
            "git has no user.email, so the key is not in an allowed signers file: commits are \
             signed, but `git log --show-signature` cannot verify them here. Run git-setup \
             again after setting user.email to add it."
        ),
    }
    Ok(())
}

fn pick_key<'a>(keys: &'a [SshKeyEntry], wanted: Option<&str>) -> Result<&'a SshKeyEntry> {
    let Some(wanted) = wanted else {
        return match keys {
            [key] => Ok(key),
            [] => bail!("there are no SSH keys in the vault"),
            _ => {
                let names: Vec<_> = keys
                    .iter()
                    .map(|k| format!("  {}  {}", k.name, k.id))
                    .collect();
                bail!(
                    "the vault has {} SSH keys; pick one with --key:\n{}",
                    keys.len(),
                    names.join("\n")
                )
            }
        };
    };
    let lower = wanted.to_lowercase();
    let matches: Vec<_> = keys
        .iter()
        .filter(|k| k.id == wanted || k.name.to_lowercase() == lower)
        .collect();
    match matches[..] {
        [key] => Ok(key),
        [] => bail!("no SSH key named {wanted:?}"),
        _ => bail!(
            "{} SSH keys are named {wanted:?}; pass the id instead",
            matches.len()
        ),
    }
}

fn reference_request(reference: &Reference) -> Request {
    Request::ItemGet {
        folder: reference.folder.clone(),
        item: reference.item.clone(),
        field: reference.field.clone(),
    }
}

fn print_secret(value: &str, no_newline: bool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(value.as_bytes())?;
    if !no_newline {
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}

/// Resolves `nw://` references in the environment (inherited, then `--env-file`s) and
/// replaces this process with `command`.
async fn run_command(
    client: &mut Client,
    env_files: &[PathBuf],
    command: &[OsString],
) -> Result<()> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for path in env_files {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        for (key, value) in
            parse_env_file(&text).with_context(|| format!("in {}", path.display()))?
        {
            env.retain(|(k, _)| *k != *key);
            env.push((key.into(), value.into()));
        }
    }

    let mut resolved = HashMap::<String, Zeroizing<String>>::new();
    for (key, value) in &mut env {
        let Some(text) = value.to_str().filter(|v| Reference::is_reference(v)) else {
            continue;
        };
        let reference: Reference = text
            .parse()
            .with_context(|| format!("in ${}", key.to_string_lossy()))?;
        if !resolved.contains_key(text) {
            let result: ValueResult = call_vault(client, reference_request(&reference))
                .await
                .with_context(|| format!("resolving ${}", key.to_string_lossy()))?;
            resolved.insert(text.to_owned(), result.value);
        }
        *value = OsString::from(resolved[text].as_str());
    }
    drop(resolved);

    let (program, args) = command.split_first().context("no command given")?;
    let error = process::exec(
        std::process::Command::new(program)
            .args(args)
            .env_clear()
            .envs(env),
    );
    Err(anyhow!("cannot run {}: {error}", program.to_string_lossy()))
}

/// `KEY=VALUE` lines; blank lines and `#` comments are skipped, a leading `export ` is
/// allowed, and one pair of matching quotes around the value is removed.
fn parse_env_file(text: &str) -> Result<Vec<(String, String)>> {
    let mut vars = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .with_context(|| format!("line {}: expected KEY=VALUE", n + 1))?;
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            bail!("line {}: bad variable name {key:?}", n + 1);
        }
        let value = value.trim();
        let value = ['"', '\'']
            .iter()
            .find_map(|q| value.strip_prefix(*q)?.strip_suffix(*q))
            .unwrap_or(value);
        vars.push((key.to_owned(), value.to_owned()));
    }
    Ok(vars)
}

fn print_items(items: &[ItemInfo]) {
    if items.is_empty() {
        eprintln!("No items.");
        return;
    }
    for item in items {
        let mut line = format!("{}  [{}]", item.name, item.kind);
        if !item.subtitle.is_empty() {
            line.push_str(&format!("  {}", item.subtitle));
        }
        if let Some(folder) = &item.folder {
            line.push_str(&format!("  ({folder})"));
        }
        println!("{line}  {}", item.id);
    }
}

async fn two_factor(
    client: &mut Client,
    offered: &[TwoFactorProvider],
    preferred: Option<Method>,
) -> Result<StatusResult> {
    // Not two-step login: the server already emailed a code to confirm this device.
    if offered == [TwoFactorProvider::NewDevice] {
        eprintln!(
            "Bitwarden does not know this device yet; it emailed a verification code. \
             Enter it, or leave it empty to have another one sent."
        );
        loop {
            let code = Zeroizing::new(prompt_tty("Verification code: ")?);
            if code.trim().is_empty() {
                client.call::<()>(Request::SendEmailCode).await?;
                eprintln!("Another code was sent. Use the newest one.");
                continue;
            }
            let provider = TwoFactorProvider::NewDevice;
            let token = Zeroizing::new(code.trim().to_owned());
            return Ok(client.call(Request::Login2fa { provider, token }).await?);
        }
    }
    let provider = match preferred {
        Some(Method::Authenticator) => TwoFactorProvider::Authenticator,
        Some(Method::Email) => TwoFactorProvider::Email,
        // Authenticator first: it needs no round trip through the mailbox.
        None if offered.contains(&TwoFactorProvider::Authenticator) => {
            TwoFactorProvider::Authenticator
        }
        None => TwoFactorProvider::Email,
    };
    if !offered.contains(&provider) {
        bail!("the server does not offer that two-step method (offered: {offered:?})");
    }
    if provider == TwoFactorProvider::Email {
        client.call::<()>(Request::SendEmailCode).await?;
        eprintln!("A verification code was sent to your email. Use the newest one.");
    }
    let prompt = match provider {
        TwoFactorProvider::Authenticator => "Authenticator code: ",
        TwoFactorProvider::Email | TwoFactorProvider::NewDevice => "Email code: ",
    };
    let token = Zeroizing::new(prompt_tty(prompt)?);
    Ok(client.call(Request::Login2fa { provider, token }).await?)
}

fn read_secret(prompt: &str, from_stdin: bool) -> Result<Zeroizing<String>> {
    if from_stdin {
        let mut line = Zeroizing::new(String::new());
        std::io::stdin().lock().read_line(&mut line)?;
        let len = line.trim_end_matches(['\r', '\n']).len();
        line.truncate(len);
        return Ok(line);
    }
    if !std::io::stdin().is_terminal() {
        bail!("no terminal to read the password from; use --password-stdin");
    }
    Ok(terminal::read_password(prompt)?)
}

/// Reads a visible line from the controlling terminal (stdin may be carrying a password).
fn prompt_tty(prompt: &str) -> Result<String> {
    terminal::read_line(prompt).map_err(|e| anyhow!("no terminal to read the code from: {e}"))
}

fn print_status(status: &StatusResult) {
    let state = match status.state {
        LockState::LoggedOut => {
            match status.email.as_deref().filter(|_| status.session_ended) {
                Some(email) => println!(
                    "Not logged in: the server ended the login of {email}; run `nibwarden login` again."
                ),
                None => println!("Not logged in."),
            }
            return;
        }
        LockState::Locked => "locked",
        LockState::Unlocked => "unlocked",
    };
    println!(
        "Logged in as {} on {}",
        status.email.as_deref().unwrap_or("?"),
        status.server.as_deref().unwrap_or("?")
    );
    println!("Vault:     {state}");
    let last_sync = status.last_sync.map_or_else(
        || "not since the daemon started".to_owned(),
        |t| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            format!("{} ago", human_duration(now.saturating_sub(t)))
        },
    );
    println!("Last sync: {last_sync}");
    let auto_lock = status.lock_timeout.map_or_else(
        || "never".to_owned(),
        |s| format!("after {} idle", human_duration(s)),
    );
    println!("Auto-lock: {auto_lock}");
    if let Some(socket) = &status.ssh_agent_socket {
        println!("SSH agent: {socket}");
    }
}

fn human_duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h{}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command and option is in the man page (packaging/man/nibwarden.1).
    #[test]
    fn man_page_covers_the_commands() {
        let page = include_str!("../../../packaging/man/nibwarden.1").replace("\\-", "-");
        fn walk(command: &clap::Command, path: &str, page: &str, missing: &mut Vec<String>) {
            for arg in command.get_arguments() {
                if let Some(long) = arg.get_long()
                    && !["help", "version"].contains(&long)
                    && !page.contains(&format!("--{long}"))
                {
                    missing.push(format!("{path} --{long}"));
                }
            }
            for sub in command.get_subcommands() {
                let path = format!("{path} {}", sub.get_name()).trim().to_owned();
                if sub.get_name() != "help" {
                    if !page.contains(&format!(".B {path}")) {
                        missing.push(path.clone());
                    }
                    walk(sub, &path, page, missing);
                }
            }
        }
        let mut missing = Vec::new();
        walk(
            &<Cli as clap::CommandFactory>::command(),
            "",
            &page,
            &mut missing,
        );
        assert!(missing.is_empty(), "not in nibwarden.1: {missing:?}");
    }

    #[test]
    fn env_file() {
        let vars =
            parse_env_file("# comment\n\nexport A=1\nB = \"nw://x/password\"\nC='a b'\nD=x=y\n")
                .unwrap();
        assert_eq!(
            vars,
            [
                ("A".into(), "1".into()),
                ("B".into(), "nw://x/password".into()),
                ("C".into(), "a b".into()),
                ("D".into(), "x=y".into()),
            ]
        );
        assert!(parse_env_file("novalue\n").is_err());
        assert!(parse_env_file("BAD KEY=1\n").is_err());
    }

    #[test]
    fn key_choice() {
        let key = |id: &str, name: &str| SshKeyEntry {
            id: id.into(),
            name: name.into(),
            public_key: String::new(),
            fingerprint: String::new(),
        };
        let one = [key("1", "Work")];
        assert_eq!(pick_key(&one, None).unwrap().id, "1");
        let many = [key("1", "Work"), key("2", "Home"), key("3", "home")];
        assert!(
            pick_key(&many, None)
                .unwrap_err()
                .to_string()
                .contains("--key")
        );
        assert_eq!(pick_key(&many, Some("work")).unwrap().id, "1");
        assert_eq!(pick_key(&many, Some("2")).unwrap().id, "2");
        assert!(pick_key(&many, Some("HOME")).is_err());
        assert!(pick_key(&many, Some("nope")).is_err());
    }
}
